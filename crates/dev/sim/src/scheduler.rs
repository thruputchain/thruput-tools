use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{ChaosEventKind, SimulationManifest, WorkloadKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseStart {
    pub phase_index: usize,
    pub simulation_time_ms: u64,
    pub duration_ms: u64,
    pub users: u32,
    pub rate_per_user_millihz: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledActorTick {
    pub phase_index: usize,
    pub actor_id: u32,
    pub actor_sequence: u64,
    pub simulation_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledChaos {
    pub chaos_index: usize,
    pub simulation_time_ms: u64,
    pub kind: ChaosEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScheduledSimulationEvent {
    ActorTick(ScheduledActorTick),
    Chaos(ScheduledChaos),
}

impl ScheduledSimulationEvent {
    pub fn simulation_time_ms(&self) -> u64 {
        match self {
            Self::ActorTick(tick) => tick.simulation_time_ms,
            Self::Chaos(chaos) => chaos.simulation_time_ms,
        }
    }

    fn deterministic_order(&self) -> (u8, usize, u32, u64) {
        match self {
            Self::Chaos(chaos) => (0, chaos.chaos_index, 0, 0),
            Self::ActorTick(tick) => (1, tick.phase_index, tick.actor_id, tick.actor_sequence),
        }
    }
}

pub fn phase_starts(manifest: &SimulationManifest) -> Result<Vec<PhaseStart>, ScheduleError> {
    let mut simulation_time_ms = 0_u64;
    let mut starts = Vec::with_capacity(manifest.load_shape.len());

    for (phase_index, phase) in manifest.load_shape.iter().enumerate() {
        let duration_ms = phase.duration.parse_millis()?;
        starts.push(PhaseStart {
            phase_index,
            simulation_time_ms,
            duration_ms,
            users: phase.users,
            rate_per_user_millihz: rate_to_millihz(phase.rate_per_user_hz)?,
        });
        simulation_time_ms = simulation_time_ms
            .checked_add(duration_ms)
            .ok_or(ScheduleError::ScheduleTooLong)?;
    }

    Ok(starts)
}

pub fn generate_schedule(
    manifest: &SimulationManifest,
) -> Result<Vec<ScheduledSimulationEvent>, ScheduleError> {
    let starts = phase_starts(manifest)?;
    let mut events = Vec::new();

    for phase_start in starts {
        let phase = &manifest.load_shape[phase_start.phase_index];
        let rate_per_user_millihz = rate_to_millihz(phase.rate_per_user_hz)?;
        let tick_count = ticks_in_phase(phase_start.duration_ms, rate_per_user_millihz)?;

        for actor_id in 0..phase.users {
            for actor_sequence in 0..tick_count {
                let offset_ms = tick_offset_ms(actor_sequence, rate_per_user_millihz)?;
                if offset_ms >= phase_start.duration_ms {
                    continue;
                }
                let simulation_time_ms = phase_start
                    .simulation_time_ms
                    .checked_add(offset_ms)
                    .ok_or(ScheduleError::ScheduleTooLong)?;
                events.push(ScheduledSimulationEvent::ActorTick(ScheduledActorTick {
                    phase_index: phase_start.phase_index,
                    actor_id,
                    actor_sequence,
                    simulation_time_ms,
                }));
            }
        }
    }

    for (chaos_index, chaos) in manifest.chaos.iter().enumerate() {
        events.push(ScheduledSimulationEvent::Chaos(ScheduledChaos {
            chaos_index,
            simulation_time_ms: chaos.at.parse_millis()?,
            kind: chaos.kind,
        }));
    }

    events.sort_by(|a, b| {
        a.simulation_time_ms()
            .cmp(&b.simulation_time_ms())
            .then_with(|| a.deterministic_order().cmp(&b.deterministic_order()))
    });

    Ok(events)
}

pub fn select_workload_for_tick(
    manifest: &SimulationManifest,
    tick: &ScheduledActorTick,
) -> Result<WorkloadKind, ScheduleError> {
    let total_weight = total_workload_weight(manifest)?;
    Ok(select_workload(
        &manifest.seed,
        &manifest.name,
        tick.phase_index,
        tick.actor_id,
        tick.actor_sequence,
        &manifest.workloads,
        total_weight,
    ))
}

fn total_workload_weight(manifest: &SimulationManifest) -> Result<u32, ScheduleError> {
    let total_weight = manifest
        .workloads
        .iter()
        .map(|workload| workload.weight)
        .try_fold(0_u32, |acc, weight| {
            acc.checked_add(weight).ok_or(ScheduleError::WeightOverflow)
        })?;
    if total_weight == 0 {
        return Err(ScheduleError::NoWorkloads);
    }
    Ok(total_weight)
}

fn ticks_in_phase(duration_ms: u64, rate_per_user_millihz: u64) -> Result<u64, ScheduleError> {
    let ticks = u128::from(duration_ms)
        .checked_mul(u128::from(rate_per_user_millihz))
        .ok_or(ScheduleError::ScheduleTooLong)?
        .div_ceil(1_000_000);
    u64::try_from(ticks).map_err(|_| ScheduleError::ScheduleTooLong)
}

fn tick_offset_ms(actor_sequence: u64, rate_per_user_millihz: u64) -> Result<u64, ScheduleError> {
    let offset = u128::from(actor_sequence)
        .checked_mul(1_000_000)
        .ok_or(ScheduleError::ScheduleTooLong)?
        / u128::from(rate_per_user_millihz);
    u64::try_from(offset).map_err(|_| ScheduleError::ScheduleTooLong)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn rate_to_millihz(rate_per_user_hz: f64) -> Result<u64, ScheduleError> {
    if !rate_per_user_hz.is_finite() || rate_per_user_hz <= 0.0 {
        return Err(ScheduleError::InvalidRate(rate_per_user_hz));
    }

    let millihz = (rate_per_user_hz * 1_000.0).round();
    if millihz > u64::MAX as f64 {
        return Err(ScheduleError::ScheduleTooLong);
    }
    if millihz < 1.0 {
        return Err(ScheduleError::RateBelowResolution(rate_per_user_hz));
    }
    Ok(millihz as u64)
}

fn select_workload(
    seed: &str,
    scenario_name: &str,
    phase_index: usize,
    actor_id: u32,
    actor_sequence: u64,
    workloads: &[crate::manifest::WorkloadSpec],
    total_weight: u32,
) -> WorkloadKind {
    let mut hasher = Sha256::new();
    hasher.update(b"hoglab workload selection v1");
    hasher.update(seed.as_bytes());
    hasher.update(scenario_name.as_bytes());
    let phase_index = u64::try_from(phase_index).expect("bug: phase index should fit u64");
    hasher.update(phase_index.to_le_bytes());
    hasher.update(actor_id.to_le_bytes());
    hasher.update(actor_sequence.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let mut pick = u32::try_from(u64::from_le_bytes(bytes) % u64::from(total_weight))
        .expect("bug: modulo total_weight must fit u32");

    for workload in workloads {
        if pick < workload.weight {
            return workload.name;
        }
        pick -= workload.weight;
    }

    workloads
        .last()
        .expect("bug: total_weight must imply at least one workload")
        .name
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("{0}")]
    Duration(#[from] crate::manifest::DurationParseError),
    #[error("load shape or chaos schedule is too long")]
    ScheduleTooLong,
    #[error("workload weights overflow u32")]
    WeightOverflow,
    #[error("simulation has no weighted workloads")]
    NoWorkloads,
    #[error("rate per user must be a positive finite number, got {0}")]
    InvalidRate(f64),
    #[error("rate per user {0} Hz is below the scheduler's 0.001 Hz resolution")]
    RateBelowResolution(f64),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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
    fn rejects_rates_below_millihz_resolution() {
        let mut manifest = manifest();
        manifest.load_shape[0].rate_per_user_hz = 0.0004;

        let err = generate_schedule(&manifest).expect_err("tiny rate should reject");
        assert!(matches!(err, ScheduleError::RateBelowResolution(_)));
    }

    #[test]
    fn restart_mixed_catalog_keeps_submit_before_restart_and_receipt_after() {
        let manifest: SimulationManifest = serde_json::from_str(include_str!(
            "../../../../examples/sim/local-restart-mixed-artifact.json"
        ))
        .expect("catalog manifest should parse");
        let events = generate_schedule(&manifest).expect("schedule should generate");

        let mut has_submit_before_restart = false;
        let mut has_receipt_after_restart = false;
        for event in events {
            let ScheduledSimulationEvent::ActorTick(tick) = event else {
                continue;
            };
            let workload =
                select_workload_for_tick(&manifest, &tick).expect("workload should select");
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
            "../../../../examples/sim/local-fibonacci-contract-call-events-smoke.json"
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
}
