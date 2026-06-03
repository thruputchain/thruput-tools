use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{ChaosEventKind, SimulationManifest, WorkloadKind};

pub const DEFAULT_RESOURCE_SAMPLE_INTERVAL_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseStart {
    pub phase_index: usize,
    pub simulation_time_ms: u64,
    pub duration_ms: u64,
    pub users: u32,
    pub rate_per_user_millihz: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stagger_actors: bool,
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
#[serde(rename_all = "camelCase")]
pub struct ScheduledResourceSample {
    pub resource_sample_index: u64,
    pub simulation_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScheduledSimulationEvent {
    ActorTick(ScheduledActorTick),
    Chaos(ScheduledChaos),
    ResourceSample(ScheduledResourceSample),
}

impl ScheduledSimulationEvent {
    pub fn simulation_time_ms(&self) -> u64 {
        match self {
            Self::ActorTick(tick) => tick.simulation_time_ms,
            Self::Chaos(chaos) => chaos.simulation_time_ms,
            Self::ResourceSample(sample) => sample.simulation_time_ms,
        }
    }

    fn deterministic_order(&self) -> (u8, usize, u32, u64) {
        match self {
            Self::Chaos(chaos) => (0, chaos.chaos_index, 0, 0),
            Self::ResourceSample(sample) => (1, 0, 0, sample.resource_sample_index),
            Self::ActorTick(tick) => (2, tick.phase_index, tick.actor_id, tick.actor_sequence),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleSummary {
    pub scheduled_workloads: u64,
    pub shutdown_simulation_time_ms: u64,
}

#[derive(Debug)]
pub struct ScheduleCursor {
    phase_starts: Vec<PhaseStart>,
    phase_tick_counts: Vec<u64>,
    next_phase_index: usize,
    actor_ticks: BinaryHeap<Reverse<ActorTickCursor>>,
    chaos: Vec<ScheduledChaos>,
    next_chaos_index: usize,
    resource_sample_interval_ms: u64,
    resource_sample_end_ms: u64,
    next_resource_sample_index: u64,
}

impl ScheduleCursor {
    pub fn new(manifest: &SimulationManifest) -> Result<Self, ScheduleError> {
        let phase_starts = phase_starts(manifest)?;
        let phase_tick_counts = phase_starts
            .iter()
            .map(|phase_start| {
                ticks_in_phase(phase_start.duration_ms, phase_start.rate_per_user_millihz)
            })
            .collect::<Result<Vec<_>, ScheduleError>>()?;
        let mut cursor = Self {
            phase_starts,
            phase_tick_counts,
            next_phase_index: 0,
            actor_ticks: BinaryHeap::new(),
            chaos: scheduled_chaos(manifest)?,
            next_chaos_index: 0,
            resource_sample_interval_ms: resource_sample_interval_ms(manifest)?,
            resource_sample_end_ms: total_simulation_duration_ms(manifest)?,
            next_resource_sample_index: 0,
        };
        cursor.fill_next_actor_phase()?;
        Ok(cursor)
    }

    fn fill_next_actor_phase(&mut self) -> Result<(), ScheduleError> {
        while self.actor_ticks.is_empty() && self.next_phase_index < self.phase_starts.len() {
            let phase_start = self.phase_starts[self.next_phase_index].clone();
            let tick_count = self.phase_tick_counts[self.next_phase_index];
            self.next_phase_index += 1;
            if tick_count == 0 || phase_start.users == 0 {
                continue;
            }
            for actor_id in 0..phase_start.users {
                self.push_actor_tick(
                    phase_start.phase_index,
                    actor_id,
                    0,
                    tick_count,
                    phase_start.simulation_time_ms,
                    phase_start.duration_ms,
                    phase_start.rate_per_user_millihz,
                    phase_start.users,
                    phase_start.stagger_actors,
                )?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_actor_tick(
        &mut self,
        phase_index: usize,
        actor_id: u32,
        actor_sequence: u64,
        tick_count: u64,
        phase_start_time_ms: u64,
        phase_duration_ms: u64,
        rate_per_user_millihz: u64,
        users: u32,
        stagger_actors: bool,
    ) -> Result<(), ScheduleError> {
        if actor_sequence >= tick_count {
            return Ok(());
        }
        let offset_ms = actor_tick_offset_ms(
            actor_sequence,
            actor_id,
            users,
            rate_per_user_millihz,
            stagger_actors,
        )?;
        if offset_ms >= phase_duration_ms {
            return Ok(());
        }
        let simulation_time_ms = phase_start_time_ms
            .checked_add(offset_ms)
            .ok_or(ScheduleError::ScheduleTooLong)?;
        self.actor_ticks.push(Reverse(ActorTickCursor {
            tick: ScheduledActorTick {
                phase_index,
                actor_id,
                actor_sequence,
                simulation_time_ms,
            },
            tick_count,
            phase_start_time_ms,
            phase_duration_ms,
            rate_per_user_millihz,
            users,
            stagger_actors,
        }));
        Ok(())
    }
}

impl Iterator for ScheduleCursor {
    type Item = ScheduledSimulationEvent;

    fn next(&mut self) -> Option<Self::Item> {
        if self.actor_ticks.is_empty() {
            self.fill_next_actor_phase()
                .expect("bug: validated schedule cursor phase should fit");
        }

        let next_actor = self
            .actor_ticks
            .peek()
            .map(|Reverse(actor)| actor.tick.clone());
        let next_chaos = self.chaos.get(self.next_chaos_index).cloned();
        let next_resource = self.next_resource_sample();

        let mut pick = (
            NextScheduleStream::None,
            (u64::MAX, u8::MAX, usize::MAX, u32::MAX, u64::MAX),
        );
        if let Some(chaos) = &next_chaos {
            let key = (chaos.simulation_time_ms, 0, chaos.chaos_index, 0, 0);
            if key < pick.1 {
                pick = (NextScheduleStream::Chaos, key);
            }
        }
        if let Some(resource) = &next_resource {
            let key = (
                resource.simulation_time_ms,
                1,
                0,
                0,
                resource.resource_sample_index,
            );
            if key < pick.1 {
                pick = (NextScheduleStream::Resource, key);
            }
        }
        if let Some(actor) = &next_actor {
            let key = (
                actor.simulation_time_ms,
                2,
                actor.phase_index,
                actor.actor_id,
                actor.actor_sequence,
            );
            if key < pick.1 {
                pick = (NextScheduleStream::Actor, key);
            }
        }

        match pick.0 {
            NextScheduleStream::Chaos => {
                self.next_chaos_index += 1;
                next_chaos.map(ScheduledSimulationEvent::Chaos)
            }
            NextScheduleStream::Resource => {
                self.next_resource_sample_index += 1;
                next_resource.map(ScheduledSimulationEvent::ResourceSample)
            }
            NextScheduleStream::Actor => self.pop_actor_tick(),
            NextScheduleStream::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NextScheduleStream {
    Chaos,
    Resource,
    Actor,
    None,
}

impl ScheduleCursor {
    fn next_resource_sample(&self) -> Option<ScheduledResourceSample> {
        let simulation_time_ms = self
            .next_resource_sample_index
            .checked_mul(self.resource_sample_interval_ms)?;
        if simulation_time_ms > self.resource_sample_end_ms {
            return None;
        }
        Some(ScheduledResourceSample {
            resource_sample_index: self.next_resource_sample_index,
            simulation_time_ms,
        })
    }

    fn pop_actor_tick(&mut self) -> Option<ScheduledSimulationEvent> {
        let Reverse(actor) = self.actor_ticks.pop()?;
        let tick = actor.tick.clone();
        self.push_actor_tick(
            actor.tick.phase_index,
            actor.tick.actor_id,
            actor.tick.actor_sequence + 1,
            actor.tick_count,
            actor.phase_start_time_ms,
            actor.phase_duration_ms,
            actor.rate_per_user_millihz,
            actor.users,
            actor.stagger_actors,
        )
        .expect("bug: validated schedule cursor tick should fit");
        Some(ScheduledSimulationEvent::ActorTick(tick))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActorTickCursor {
    tick: ScheduledActorTick,
    tick_count: u64,
    phase_start_time_ms: u64,
    phase_duration_ms: u64,
    rate_per_user_millihz: u64,
    users: u32,
    stagger_actors: bool,
}

impl Ord for ActorTickCursor {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tick
            .simulation_time_ms
            .cmp(&other.tick.simulation_time_ms)
            .then_with(|| self.tick.phase_index.cmp(&other.tick.phase_index))
            .then_with(|| self.tick.actor_id.cmp(&other.tick.actor_id))
            .then_with(|| self.tick.actor_sequence.cmp(&other.tick.actor_sequence))
    }
}

impl PartialOrd for ActorTickCursor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
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
            stagger_actors: phase.stagger_actors,
        });
        simulation_time_ms = simulation_time_ms
            .checked_add(duration_ms)
            .ok_or(ScheduleError::ScheduleTooLong)?;
    }

    Ok(starts)
}

pub fn schedule_summary(manifest: &SimulationManifest) -> Result<ScheduleSummary, ScheduleError> {
    let starts = phase_starts(manifest)?;
    let mut scheduled_workloads = 0_u64;
    let mut shutdown_simulation_time_ms = 0_u64;

    for phase_start in &starts {
        let tick_count =
            ticks_in_phase(phase_start.duration_ms, phase_start.rate_per_user_millihz)?;
        for actor_id in 0..phase_start.users {
            let actor_tick_count = if phase_start.stagger_actors {
                let stagger_offset = actor_stagger_offset_ms(
                    actor_id,
                    phase_start.users,
                    phase_start.rate_per_user_millihz,
                )?;
                if stagger_offset >= phase_start.duration_ms {
                    0
                } else {
                    ticks_in_phase(
                        phase_start.duration_ms - stagger_offset,
                        phase_start.rate_per_user_millihz,
                    )?
                }
            } else {
                tick_count
            };
            scheduled_workloads = scheduled_workloads
                .checked_add(actor_tick_count)
                .ok_or(ScheduleError::ScheduleTooLong)?;
            if actor_tick_count > 0 {
                let last_offset = actor_tick_offset_ms(
                    actor_tick_count - 1,
                    actor_id,
                    phase_start.users,
                    phase_start.rate_per_user_millihz,
                    phase_start.stagger_actors,
                )?;
                if last_offset < phase_start.duration_ms {
                    shutdown_simulation_time_ms = shutdown_simulation_time_ms.max(
                        phase_start
                            .simulation_time_ms
                            .checked_add(last_offset)
                            .ok_or(ScheduleError::ScheduleTooLong)?,
                    );
                }
            }
        }
    }

    for chaos in scheduled_chaos(manifest)? {
        shutdown_simulation_time_ms = shutdown_simulation_time_ms.max(chaos.simulation_time_ms);
    }
    if let Some(last_resource_sample_time_ms) = last_resource_sample_time_ms(manifest)? {
        shutdown_simulation_time_ms = shutdown_simulation_time_ms.max(last_resource_sample_time_ms);
    }

    Ok(ScheduleSummary {
        scheduled_workloads,
        shutdown_simulation_time_ms,
    })
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
                let offset_ms = actor_tick_offset_ms(
                    actor_sequence,
                    actor_id,
                    phase.users,
                    rate_per_user_millihz,
                    phase.stagger_actors,
                )?;
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
    events.extend(
        scheduled_resource_samples(manifest)?
            .into_iter()
            .map(ScheduledSimulationEvent::ResourceSample),
    );

    events.sort_by(|a, b| {
        a.simulation_time_ms()
            .cmp(&b.simulation_time_ms())
            .then_with(|| a.deterministic_order().cmp(&b.deterministic_order()))
    });

    Ok(events)
}

fn scheduled_chaos(manifest: &SimulationManifest) -> Result<Vec<ScheduledChaos>, ScheduleError> {
    let mut chaos = manifest
        .chaos
        .iter()
        .enumerate()
        .map(|(chaos_index, chaos)| {
            Ok(ScheduledChaos {
                chaos_index,
                simulation_time_ms: chaos.at.parse_millis()?,
                kind: chaos.kind,
            })
        })
        .collect::<Result<Vec<_>, ScheduleError>>()?;
    chaos.sort_by_key(|chaos| (chaos.simulation_time_ms, chaos.chaos_index));
    Ok(chaos)
}

fn scheduled_resource_samples(
    manifest: &SimulationManifest,
) -> Result<Vec<ScheduledResourceSample>, ScheduleError> {
    let interval_ms = resource_sample_interval_ms(manifest)?;
    let end_ms = total_simulation_duration_ms(manifest)?;
    let count = end_ms
        .checked_div(interval_ms)
        .ok_or(ScheduleError::ScheduleTooLong)?
        .checked_add(1)
        .ok_or(ScheduleError::ScheduleTooLong)?;
    (0..count)
        .map(|resource_sample_index| {
            Ok(ScheduledResourceSample {
                resource_sample_index,
                simulation_time_ms: resource_sample_index
                    .checked_mul(interval_ms)
                    .ok_or(ScheduleError::ScheduleTooLong)?,
            })
        })
        .collect()
}

pub fn resource_sample_interval_ms(manifest: &SimulationManifest) -> Result<u64, ScheduleError> {
    manifest
        .observability
        .as_ref()
        .and_then(|observability| observability.resource_sample_interval.as_ref())
        .map_or(Ok(DEFAULT_RESOURCE_SAMPLE_INTERVAL_MS), |interval| {
            interval.parse_millis().map_err(ScheduleError::Duration)
        })
}

fn total_simulation_duration_ms(manifest: &SimulationManifest) -> Result<u64, ScheduleError> {
    manifest.load_shape.iter().try_fold(0_u64, |total, phase| {
        total
            .checked_add(phase.duration.parse_millis()?)
            .ok_or(ScheduleError::ScheduleTooLong)
    })
}

fn last_resource_sample_time_ms(
    manifest: &SimulationManifest,
) -> Result<Option<u64>, ScheduleError> {
    let end_ms = total_simulation_duration_ms(manifest)?;
    let interval_ms = resource_sample_interval_ms(manifest)?;
    // DurationSpec::parse_millis rejects zero, so the truncating division is safe.
    Ok(Some((end_ms / interval_ms) * interval_ms))
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

fn actor_tick_offset_ms(
    actor_sequence: u64,
    actor_id: u32,
    users: u32,
    rate_per_user_millihz: u64,
    stagger_actors: bool,
) -> Result<u64, ScheduleError> {
    let base_offset = tick_offset_ms(actor_sequence, rate_per_user_millihz)?;
    if !stagger_actors {
        return Ok(base_offset);
    }
    base_offset
        .checked_add(actor_stagger_offset_ms(
            actor_id,
            users,
            rate_per_user_millihz,
        )?)
        .ok_or(ScheduleError::ScheduleTooLong)
}

fn actor_stagger_offset_ms(
    actor_id: u32,
    users: u32,
    rate_per_user_millihz: u64,
) -> Result<u64, ScheduleError> {
    if users <= 1 || actor_id == 0 {
        return Ok(0);
    }
    let denominator = u128::from(rate_per_user_millihz)
        .checked_mul(u128::from(users))
        .ok_or(ScheduleError::ScheduleTooLong)?;
    let offset = u128::from(actor_id)
        .checked_mul(1_000_000)
        .ok_or(ScheduleError::ScheduleTooLong)?
        / denominator;
    u64::try_from(offset).map_err(|_| ScheduleError::ScheduleTooLong)
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
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
