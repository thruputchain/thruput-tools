use std::collections::VecDeque;
use std::time::Instant;

use groundhog_rpc_api_types::{receipt::TransactionReceipt, transaction::TxHash};
use groundhog_signing::LocalSigner;
use groundhog_xdr::types::ContractAddress;

use crate::manifest::SimulationManifest;
use crate::nonce::NonceNamespace;
use crate::runtime::SimAccount;

/// Mutable per-actor state owned by the simulator runtime.
#[derive(Debug)]
pub struct ActorState {
    pub actor_id: u32,
    pub account_index: u32,
    pub signer: LocalSigner,
    pub wallet_address: ContractAddress,
    pub nonce_namespace: NonceNamespace,
    pub next_nonce_sequence: u32,
    pub last_submitted_tx_hash: Option<TxHash>,
    pub last_submitted_scheduled_time_ms: Option<u64>,
    pub last_submitted_completed_at: Option<Instant>,
    pub last_observed_receipt: Option<TransactionReceipt>,
    pending_receipts: VecDeque<PendingReceipt>,
}

impl ActorState {
    /// Build actor state from a derived bootstrap account.
    pub fn new(actor_id: u32, account: SimAccount, nonce_namespace: NonceNamespace) -> Self {
        Self {
            actor_id,
            account_index: account.index,
            signer: account.signer,
            wallet_address: account.wallet_address,
            nonce_namespace,
            next_nonce_sequence: 0,
            last_submitted_tx_hash: None,
            last_submitted_scheduled_time_ms: None,
            last_submitted_completed_at: None,
            last_observed_receipt: None,
            pending_receipts: VecDeque::new(),
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
    ) -> Result<(), ActorError> {
        self.next_nonce_sequence =
            self.next_nonce_sequence
                .checked_add(1)
                .ok_or(ActorError::NonceSequenceExhausted {
                    actor_id: self.actor_id,
                })?;
        self.last_submitted_tx_hash = Some(tx_hash);
        self.last_submitted_scheduled_time_ms = Some(scheduled_time_ms);
        self.last_submitted_completed_at = Some(completed_at);
        self.pending_receipts.push_back(PendingReceipt {
            tx_hash,
            submit_completed_at: completed_at,
        });
        Ok(())
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

    /// Return the next submitted transaction that still needs a receipt.
    pub fn receipt_poll_target(&self) -> Option<ReceiptPollTarget> {
        self.pending_receipts
            .front()
            .map(|pending| ReceiptPollTarget {
                tx_hash: pending.tx_hash,
                submit_completed_at: pending.submit_completed_at,
            })
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

#[derive(Debug, Clone, Copy)]
struct PendingReceipt {
    tx_hash: TxHash,
    submit_completed_at: Instant,
}

/// Construct actor state for the maximum actor count requested by a scenario.
pub fn build_actor_states(
    manifest: &SimulationManifest,
    accounts: Vec<SimAccount>,
    nonce_namespace: NonceNamespace,
) -> Result<Vec<ActorState>, ActorError> {
    let actor_count = max_actor_count(manifest);
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

fn max_actor_count(manifest: &SimulationManifest) -> usize {
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
}

#[cfg(test)]
mod tests {
    use groundhog_rpc_api_types::hex::Hex0x;
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
            height: groundhog_rpc_api_types::BlockHeight::from(1),
            status: groundhog_rpc_api_types::receipt::ExecStatus::Ok,
            events: Vec::new(),
            return_data: None,
            abort_data: None,
        });

        assert_eq!(actor.receipt_poll_target().unwrap().tx_hash, tx_hash(2));
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
        }
    }

    fn tx_hash(byte: u8) -> TxHash {
        TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([byte; 32])))
    }
}
