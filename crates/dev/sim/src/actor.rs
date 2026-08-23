use std::collections::VecDeque;
use std::time::Instant;

use groundhog_json_rpc_types::{receipt::TransactionReceipt, transaction::TxHash};
use groundhog_signing::LocalSigner;
use groundhog_xdr::types::{ContractAddress, NetworkHash};

use crate::manifest::SimulationManifest;
use crate::nonce::NonceNamespace;
use crate::runtime::SimAccount;

pub const DEFAULT_PENDING_RECEIPT_LIMIT_PER_ACTOR: usize = 1_024;

/// Mutable per-actor state owned by the simulator runtime.
#[derive(Debug)]
pub struct ActorState {
    pub actor_id: u32,
    pub account_index: u32,
    pub signer: LocalSigner,
    pub wallet_address: ContractAddress,
    /// Network this actor signs for. Part of every signature preimage.
    pub network_hash: NetworkHash,
    pub nonce_namespace: NonceNamespace,
    pub next_nonce_sequence: u32,
    pub last_submitted_tx_hash: Option<TxHash>,
    pub last_submitted_scheduled_time_ms: Option<u64>,
    pub last_submitted_completed_at: Option<Instant>,
    pub last_observed_receipt: Option<TransactionReceipt>,
    pending_receipts: VecDeque<PendingReceipt>,
    pending_receipt_limit: usize,
    read_after_write: ReadAfterWriteState,
}

/// What an actor knows about its read-after-write counter on the target.
///
/// The expectation advances only when a write is known committed, so a failed
/// read-back never desyncs the actor; an unknown commit status (receipt RPC
/// failure or restart) drops to `NeedsReconcile`, which forces a pre-read to
/// rebase before the next write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadAfterWriteState {
    /// No trusted baseline yet (first tick, or after an uncertain outcome). The
    /// next tick must pre-read to rebase before writing.
    NeedsReconcile,
    /// The actor's known committed counter value.
    Known(u64),
}

impl ReadAfterWriteState {
    /// Expected counter value after one more increment.
    ///
    /// In the `Known` state the increment is provably one past the last known
    /// committed value, so no baseline is needed. In `NeedsReconcile` the caller
    /// must supply the reconcile pre-read baseline; omitting it is a usage bug.
    pub fn expected_after_increment(
        self,
        actor_id: u32,
        reconcile_baseline: Option<u64>,
    ) -> Result<u64, ActorError> {
        let base = match self {
            ReadAfterWriteState::Known(value) => value,
            ReadAfterWriteState::NeedsReconcile => {
                reconcile_baseline.ok_or(ActorError::ReadAfterWriteNeedsReconcile { actor_id })?
            }
        };
        base.checked_add(1)
            .ok_or(ActorError::ReadAfterWriteOverflow { actor_id })
    }
}

impl ActorState {
    /// Build actor state from a derived bootstrap account.
    pub fn new(actor_id: u32, account: SimAccount, nonce_namespace: NonceNamespace) -> Self {
        Self {
            actor_id,
            account_index: account.index,
            signer: account.signer,
            wallet_address: account.wallet_address,
            network_hash: account.network_hash,
            nonce_namespace,
            next_nonce_sequence: 0,
            last_submitted_tx_hash: None,
            last_submitted_scheduled_time_ms: None,
            last_submitted_completed_at: None,
            last_observed_receipt: None,
            pending_receipts: VecDeque::new(),
            pending_receipt_limit: DEFAULT_PENDING_RECEIPT_LIMIT_PER_ACTOR,
            read_after_write: ReadAfterWriteState::NeedsReconcile,
        }
    }

    /// Return the next namespaced transaction nonce without consuming it.
    pub fn current_nonce(&self) -> Result<u64, ActorError> {
        if self.next_nonce_sequence == u32::MAX {
            return Err(ActorError::NonceSequenceExhausted {
                actor_id: self.actor_id,
            });
        }
        Ok(self.nonce_namespace.make_nonce(self.next_nonce_sequence))
    }

    /// Record that this actor submitted a transaction.
    pub fn record_submitted_tx(
        &mut self,
        tx_hash: TxHash,
        scheduled_time_ms: u64,
        completed_at: Instant,
    ) -> Result<PendingReceiptRecordOutcome, ActorError> {
        self.next_nonce_sequence =
            self.next_nonce_sequence
                .checked_add(1)
                .ok_or(ActorError::NonceSequenceExhausted {
                    actor_id: self.actor_id,
                })?;
        self.last_submitted_tx_hash = Some(tx_hash);
        self.last_submitted_scheduled_time_ms = Some(scheduled_time_ms);
        self.last_submitted_completed_at = Some(completed_at);
        if self.pending_receipts.len() >= self.pending_receipt_limit {
            return Ok(PendingReceiptRecordOutcome::Dropped {
                pending_receipts: self.pending_receipts.len(),
                pending_receipt_limit: self.pending_receipt_limit,
            });
        }
        self.pending_receipts.push_back(PendingReceipt {
            tx_hash,
            submit_completed_at: completed_at,
            receipt_poll_in_flight: false,
        });
        Ok(PendingReceiptRecordOutcome::Queued {
            pending_receipts: self.pending_receipts.len(),
            pending_receipt_limit: self.pending_receipt_limit,
        })
    }

    #[cfg(test)]
    pub fn set_pending_receipt_limit_for_test(&mut self, limit: usize) {
        self.pending_receipt_limit = limit;
    }

    /// Consume one nonce for a synchronous setup transaction that is tracked
    /// outside workload receipt queues.
    pub fn consume_setup_nonce(&mut self) -> Result<(), ActorError> {
        self.next_nonce_sequence =
            self.next_nonce_sequence
                .checked_add(1)
                .ok_or(ActorError::NonceSequenceExhausted {
                    actor_id: self.actor_id,
                })?;
        Ok(())
    }

    /// Reserve the next submitted transaction that still needs a receipt poll.
    pub fn reserve_receipt_poll_target(&mut self) -> Option<ReceiptPollTarget> {
        self.pending_receipts
            .iter_mut()
            .find(|pending| !pending.receipt_poll_in_flight)
            .map(|pending| {
                pending.receipt_poll_in_flight = true;
                ReceiptPollTarget {
                    tx_hash: pending.tx_hash,
                    submit_completed_at: pending.submit_completed_at,
                }
            })
    }

    /// Return the next submitted transaction that still needs a receipt.
    #[cfg(test)]
    pub fn receipt_poll_target(&self) -> Option<ReceiptPollTarget> {
        self.pending_receipts
            .front()
            .map(|pending| ReceiptPollTarget {
                tx_hash: pending.tx_hash,
                submit_completed_at: pending.submit_completed_at,
            })
    }

    /// Release a previously reserved receipt-poll target that still needs retrying.
    pub fn release_receipt_poll_target(&mut self, tx_hash: TxHash) {
        if let Some(pending) = self
            .pending_receipts
            .iter_mut()
            .find(|pending| pending.tx_hash == tx_hash)
        {
            pending.receipt_poll_in_flight = false;
        }
    }

    /// Return the number of submitted transactions still waiting for receipts.
    pub fn pending_receipt_count(&self) -> u64 {
        self.pending_receipts.len() as u64
    }

    /// Drain submitted transactions that were still waiting for receipts.
    pub fn drain_pending_receipts(&mut self) -> Vec<DrainedPendingReceipt> {
        self.pending_receipts
            .drain(..)
            .map(|pending| DrainedPendingReceipt {
                actor_id: self.actor_id,
                tx_hash: pending.tx_hash,
                submit_completed_at: pending.submit_completed_at,
            })
            .collect()
    }

    /// Advance the actor nonce after a submit was interrupted at an unknown point.
    pub fn record_uncertain_submit_nonce(&mut self) -> Result<(), ActorError> {
        self.next_nonce_sequence =
            self.next_nonce_sequence
                .checked_add(1)
                .ok_or(ActorError::NonceSequenceExhausted {
                    actor_id: self.actor_id,
                })?;
        Ok(())
    }

    /// Record a committed receipt observed for this actor.
    pub fn record_receipt(&mut self, receipt: TransactionReceipt) {
        self.pending_receipts
            .retain(|pending| pending.tx_hash != receipt.tx_hash);
        self.last_observed_receipt = Some(receipt);
    }

    /// Return the actor's current read-after-write counter belief.
    pub fn read_after_write_state(&self) -> ReadAfterWriteState {
        self.read_after_write
    }

    /// Whether the next read-after-write tick must pre-read before writing.
    pub fn read_after_write_needs_reconcile(&self) -> bool {
        matches!(self.read_after_write, ReadAfterWriteState::NeedsReconcile)
    }

    /// Expected counter value after one more increment.
    ///
    /// In the `Known` state the increment is provably one past the last known
    /// committed value, so no baseline is needed. In `NeedsReconcile` the caller
    /// must supply the reconcile pre-read baseline; omitting it is a usage bug.
    pub fn read_after_write_expected_after_increment(
        &self,
        reconcile_baseline: Option<u64>,
    ) -> Result<u64, ActorError> {
        self.read_after_write
            .expected_after_increment(self.actor_id, reconcile_baseline)
    }

    /// Rebase the actor to a known committed counter value.
    ///
    /// Call this the moment a write is known committed (an `ok` receipt),
    /// before reading back, so a later read failure cannot leave the actor
    /// model behind.
    pub fn record_read_after_write_committed(&mut self, committed_value: u64) {
        self.read_after_write = ReadAfterWriteState::Known(committed_value);
    }

    /// Drop the actor's read-after-write belief, forcing a reconcile pre-read on
    /// the next tick. Use on read-back mismatch, unknown commit status, or
    /// restart.
    pub fn invalidate_read_after_write(&mut self) {
        self.read_after_write = ReadAfterWriteState::NeedsReconcile;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReceiptPollTarget {
    pub tx_hash: TxHash,
    pub submit_completed_at: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct DrainedPendingReceipt {
    pub actor_id: u32,
    pub tx_hash: TxHash,
    pub submit_completed_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReceiptRecordOutcome {
    Queued {
        pending_receipts: usize,
        pending_receipt_limit: usize,
    },
    Dropped {
        pending_receipts: usize,
        pending_receipt_limit: usize,
    },
}

#[derive(Debug, Clone, Copy)]
struct PendingReceipt {
    tx_hash: TxHash,
    submit_completed_at: Instant,
    receipt_poll_in_flight: bool,
}

/// Construct actor state for the maximum actor count requested by a scenario.
pub fn build_actor_states(
    manifest: &SimulationManifest,
    accounts: Vec<SimAccount>,
    nonce_namespace: NonceNamespace,
) -> Result<Vec<ActorState>, ActorError> {
    let actor_count = required_actor_count(manifest);
    let available = accounts.len();
    if actor_count > available {
        return Err(ActorError::TooManyActors {
            requested: actor_count,
            available,
        });
    }

    accounts
        .into_iter()
        .take(actor_count)
        .enumerate()
        .map(|(actor_id, account)| {
            let actor_id =
                u32::try_from(actor_id).map_err(|_| ActorError::ActorIdTooLarge { actor_id })?;
            Ok(ActorState::new(actor_id, account, nonce_namespace))
        })
        .collect()
}

pub fn build_actor_states_from_accounts(
    accounts: Vec<SimAccount>,
    nonce_namespace: NonceNamespace,
) -> Result<Vec<ActorState>, ActorError> {
    accounts
        .into_iter()
        .enumerate()
        .map(|(actor_id, account)| {
            let actor_id =
                u32::try_from(actor_id).map_err(|_| ActorError::ActorIdTooLarge { actor_id })?;
            Ok(ActorState::new(actor_id, account, nonce_namespace))
        })
        .collect()
}

pub fn required_actor_count(manifest: &SimulationManifest) -> usize {
    manifest
        .load_shape
        .iter()
        .map(|phase| phase.users as usize)
        .max()
        .unwrap_or_default()
}

/// Errors from actor-state construction and mutation.
#[derive(Debug, thiserror::Error)]
pub enum ActorError {
    #[error("scenario requests {requested} actors but only {available} accounts are available")]
    TooManyActors { requested: usize, available: usize },
    #[error("actor id {actor_id} does not fit in u32")]
    ActorIdTooLarge { actor_id: usize },
    #[error("actor {actor_id} exhausted the low 32 nonce sequence bits")]
    NonceSequenceExhausted { actor_id: u32 },
    #[error("actor {actor_id} read-after-write state needs a reconcile pre-read baseline")]
    ReadAfterWriteNeedsReconcile { actor_id: u32 },
    #[error("actor {actor_id} read-after-write counter overflowed u64")]
    ReadAfterWriteOverflow { actor_id: u32 },
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::hex::Hex0x;
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::ContractAddress;
    use serde_json::json;

    use super::*;

    #[test]
    fn actor_allocates_namespaced_nonces() {
        let mut actor = ActorState::new(
            2,
            sim_account(9),
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );

        assert_eq!(actor.current_nonce().unwrap(), 0x1234_5678_0000_0000);
        actor
            .record_submitted_tx(tx_hash(1), 10, Instant::now())
            .expect("submitted tx should record");
        assert_eq!(actor.current_nonce().unwrap(), 0x1234_5678_0000_0001);
        actor
            .record_submitted_tx(tx_hash(2), 20, Instant::now())
            .expect("submitted tx should record");
        assert_eq!(actor.next_nonce_sequence, 2);
        assert_eq!(actor.receipt_poll_target().unwrap().tx_hash, tx_hash(1));
    }

    #[test]
    fn actor_does_not_advance_nonce_until_submit_is_recorded() {
        let mut actor = ActorState::new(
            2,
            sim_account(9),
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );

        assert_eq!(actor.current_nonce().unwrap(), 0x1234_5678_0000_0000);
        assert_eq!(actor.current_nonce().unwrap(), 0x1234_5678_0000_0000);
        assert_eq!(actor.next_nonce_sequence, 0);

        actor
            .record_submitted_tx(tx_hash(3), 30, Instant::now())
            .expect("submitted tx should record");

        assert_eq!(actor.current_nonce().unwrap(), 0x1234_5678_0000_0001);
    }

    #[test]
    fn pending_receipt_tracking_drops_when_limit_is_reached() {
        let mut actor = ActorState::new(
            2,
            sim_account(9),
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        actor.set_pending_receipt_limit_for_test(1);
        let now = Instant::now();

        let first = actor
            .record_submitted_tx(tx_hash(1), 10, now)
            .expect("first submit should record");
        let second = actor
            .record_submitted_tx(tx_hash(2), 20, now)
            .expect("second submit should still advance actor state");

        assert_eq!(
            first,
            PendingReceiptRecordOutcome::Queued {
                pending_receipts: 1,
                pending_receipt_limit: 1,
            }
        );
        assert_eq!(
            second,
            PendingReceiptRecordOutcome::Dropped {
                pending_receipts: 1,
                pending_receipt_limit: 1,
            }
        );
        assert_eq!(actor.pending_receipt_count(), 1);
        assert_eq!(actor.last_submitted_tx_hash, Some(tx_hash(2)));
        assert_eq!(actor.next_nonce_sequence, 2);
    }

    #[test]
    fn actor_polls_oldest_pending_receipt_and_removes_observed_receipt() {
        let mut actor = ActorState::new(
            2,
            sim_account(9),
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        actor
            .record_submitted_tx(tx_hash(1), 10, Instant::now())
            .expect("first tx should record");
        actor
            .record_submitted_tx(tx_hash(2), 20, Instant::now())
            .expect("second tx should record");

        assert_eq!(actor.receipt_poll_target().unwrap().tx_hash, tx_hash(1));
        actor.record_receipt(TransactionReceipt {
            tx_hash: tx_hash(1),
            height: groundhog_json_rpc_types::BlockHeight::from(1),
            status: groundhog_json_rpc_types::receipt::ExecStatus::Ok,
            events: Vec::new(),
            return_data: None,
            abort_data: None,
        });

        assert_eq!(actor.receipt_poll_target().unwrap().tx_hash, tx_hash(2));
    }

    #[test]
    fn actor_reserves_distinct_receipt_poll_targets() {
        let mut actor = ActorState::new(
            2,
            sim_account(9),
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        actor
            .record_submitted_tx(tx_hash(1), 10, Instant::now())
            .expect("first tx should record");
        actor
            .record_submitted_tx(tx_hash(2), 20, Instant::now())
            .expect("second tx should record");

        let first = actor
            .reserve_receipt_poll_target()
            .expect("first receipt should reserve");
        let second = actor
            .reserve_receipt_poll_target()
            .expect("second receipt should reserve");

        assert_eq!(first.tx_hash, tx_hash(1));
        assert_eq!(second.tx_hash, tx_hash(2));
        assert!(actor.reserve_receipt_poll_target().is_none());

        actor.release_receipt_poll_target(first.tx_hash);
        assert_eq!(
            actor
                .reserve_receipt_poll_target()
                .expect("released receipt should reserve again")
                .tx_hash,
            tx_hash(1)
        );
    }

    #[test]
    fn actor_reports_nonce_sequence_exhaustion_at_boundary() {
        let mut actor = ActorState::new(
            2,
            sim_account(9),
            NonceNamespace::new(0x1234_5678).expect("namespace should fit"),
        );
        actor.next_nonce_sequence = u32::MAX;

        assert!(matches!(
            actor.current_nonce(),
            Err(ActorError::NonceSequenceExhausted { actor_id: 2 })
        ));
    }

    #[test]
    fn read_after_write_starts_needing_reconcile() {
        let actor = ActorState::new(
            0,
            sim_account(1),
            NonceNamespace::new(1).expect("namespace should fit"),
        );

        assert!(actor.read_after_write_needs_reconcile());
        assert_eq!(
            actor.read_after_write_state(),
            ReadAfterWriteState::NeedsReconcile
        );
    }

    #[test]
    fn read_after_write_reconcile_requires_baseline_then_advances() {
        let mut actor = ActorState::new(
            0,
            sim_account(1),
            NonceNamespace::new(1).expect("namespace should fit"),
        );

        // NeedsReconcile without a pre-read baseline is a usage bug.
        assert!(matches!(
            actor.read_after_write_expected_after_increment(None),
            Err(ActorError::ReadAfterWriteNeedsReconcile { actor_id: 0 })
        ));

        // Reconcile pre-read observed 5 -> next write should land 6.
        assert_eq!(
            actor
                .read_after_write_expected_after_increment(Some(5))
                .unwrap(),
            6
        );

        // A known-committed write rebases without needing another pre-read.
        actor.record_read_after_write_committed(6);
        assert_eq!(
            actor.read_after_write_state(),
            ReadAfterWriteState::Known(6)
        );
        assert!(!actor.read_after_write_needs_reconcile());
        assert_eq!(
            actor
                .read_after_write_expected_after_increment(None)
                .unwrap(),
            7
        );
    }

    #[test]
    fn read_after_write_invalidation_forces_reconcile() {
        let mut actor = ActorState::new(
            0,
            sim_account(1),
            NonceNamespace::new(1).expect("namespace should fit"),
        );
        actor.record_read_after_write_committed(3);

        actor.invalidate_read_after_write();

        assert!(actor.read_after_write_needs_reconcile());
        assert!(matches!(
            actor.read_after_write_expected_after_increment(None),
            Err(ActorError::ReadAfterWriteNeedsReconcile { .. })
        ));
    }

    #[test]
    fn read_after_write_reports_overflow_at_boundary() {
        let mut actor = ActorState::new(
            0,
            sim_account(1),
            NonceNamespace::new(1).expect("namespace should fit"),
        );
        actor.record_read_after_write_committed(u64::MAX);

        assert!(matches!(
            actor.read_after_write_expected_after_increment(None),
            Err(ActorError::ReadAfterWriteOverflow { actor_id: 0 })
        ));
    }

    #[test]
    fn actor_state_uses_one_bootstrap_account_per_actor() {
        let manifest: SimulationManifest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "actor-test",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1s", "users": 2, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest should parse");

        let actors = build_actor_states(
            &manifest,
            vec![sim_account(4), sim_account(5)],
            NonceNamespace::new(99).expect("namespace should fit"),
        )
        .expect("actors should build");

        assert_eq!(actors.len(), 2);
        assert_eq!(actors[0].actor_id, 0);
        assert_eq!(actors[0].account_index, 4);
        assert_eq!(actors[1].actor_id, 1);
        assert_eq!(actors[1].account_index, 5);
    }

    fn sim_account(index: u32) -> SimAccount {
        let address_byte = u8::try_from(index).expect("test account index should fit u8");
        SimAccount {
            index,
            signer: derive_dev_account_signer("actor-test-seed", index as usize),
            wallet_address: Hex0x(ContractAddress::from([address_byte; 32])).0,
            network_hash: groundhog_xdr::types::NetworkHash::new(
                groundhog_xdr::types::SHA256Hash::new([0x5a; 32]),
            ),
        }
    }

    fn tx_hash(byte: u8) -> TxHash {
        TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([byte; 32])))
    }
}
