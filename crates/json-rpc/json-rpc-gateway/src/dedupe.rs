//! Replay window for accepted transaction submissions.
//!
//! [`AcceptedTxCache`] remembers the canonical hashes of transactions
//! the downstream mempool path accepted. A duplicate submission inside
//! the TTL is answered with the same success response without another
//! backend round trip. Only accepted submissions are recorded — a
//! failed or timed-out submission must stay retryable — and both a TTL
//! and an entry cap bound the cache so it cannot become a memory sink
//! itself.

use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use groundhog_json_rpc_types::transaction::TxHash;
use tokio::sync::watch;

use crate::config::SubmitDedupeConfig;

type HashKey = [u8; 32];

/// Bounded, TTL'd set of recently accepted transaction hashes.
///
/// Also tracks hashes whose submission is currently in flight, so
/// concurrent duplicates coalesce onto one backend submission instead
/// of racing past the accepted window before it is populated. The
/// in-flight map is bounded by the number of concurrent submissions.
pub(crate) struct AcceptedTxCache {
    ttl: Duration,
    max_entries: usize,
    state: Mutex<CacheState>,
    in_flight: Mutex<HashMap<HashKey, watch::Receiver<InFlightOutcome>>>,
}

/// Progress of an in-flight submission of one hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InFlightOutcome {
    /// The leader has not settled yet.
    Pending,
    /// The downstream mempool path accepted the submission.
    Accepted,
    /// The submission failed; the hash is retryable again.
    Failed,
}

/// Admission decision for one submission attempt of a hash.
pub(crate) enum SubmitSlot {
    /// Accepted within the TTL window; answer idempotently.
    AlreadyAccepted,
    /// The caller owns this hash's submission; settle the guard.
    Leader(InFlightGuard),
    /// Another submission of this hash is in flight; await its outcome.
    Waiter(watch::Receiver<InFlightOutcome>),
}

/// The leader's claim on an in-flight hash.
///
/// `settle_accepted` promotes the hash into the accepted window and
/// releases waiters with success. Dropping the guard without settling
/// (any failure or panic path) releases waiters with
/// [`InFlightOutcome::Failed`] and frees the slot so a retry can lead.
pub(crate) struct InFlightGuard {
    cache: Arc<AcceptedTxCache>,
    key: HashKey,
    sender: watch::Sender<InFlightOutcome>,
    settled: bool,
}

impl InFlightGuard {
    /// Record the hash as accepted and release waiters with success.
    pub(crate) fn settle_accepted(mut self) {
        self.cache.record_accepted_at(self.key, Instant::now());
        self.release(InFlightOutcome::Accepted);
        self.settled = true;
    }

    fn release(&self, outcome: InFlightOutcome) {
        // Remove before waking waiters: a waiter that retries after
        // `Failed` must not find this settled entry still claiming the
        // hash.
        self.cache
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
        let _ = self.sender.send(outcome);
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if !self.settled {
            self.release(InFlightOutcome::Failed);
        }
    }
}

#[derive(Default)]
struct CacheState {
    /// Hash → the instant its latest accepted submission was recorded.
    entries: HashMap<HashKey, Instant>,
    /// Insertion log in arrival order; drives TTL and capacity eviction.
    /// May briefly hold stale rows for re-inserted hashes, so eviction
    /// only removes a map entry whose timestamp matches the row.
    insertion_log: VecDeque<(HashKey, Instant)>,
}

impl AcceptedTxCache {
    /// Build the cache from config, or `None` when disabled or sized to
    /// hold nothing.
    pub(crate) fn from_config(config: &SubmitDedupeConfig) -> Option<Self> {
        (config.enabled && config.max_entries > 0 && config.ttl_ms > 0)
            .then(|| Self::new(config.max_entries, Duration::from_millis(config.ttl_ms)))
    }

    pub(crate) fn new(max_entries: usize, ttl: Duration) -> Self {
        Self {
            ttl,
            max_entries,
            state: Mutex::new(CacheState::default()),
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `hash` was accepted within the TTL window.
    #[cfg(test)]
    pub(crate) fn contains(&self, hash: &TxHash) -> bool {
        self.contains_at(key(hash), Instant::now())
    }

    /// Claim one submission attempt of `hash`.
    ///
    /// Exactly one concurrent caller per hash becomes the
    /// [`SubmitSlot::Leader`]; the rest wait on the leader's outcome
    /// instead of submitting the same hash again.
    pub(crate) fn begin_submit(self: &Arc<Self>, hash: &TxHash) -> SubmitSlot {
        let key = key(hash);
        if self.contains_at(key, Instant::now()) {
            return SubmitSlot::AlreadyAccepted;
        }
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(receiver) = in_flight.get(&key) {
            return SubmitSlot::Waiter(receiver.clone());
        }
        let (sender, receiver) = watch::channel(InFlightOutcome::Pending);
        in_flight.insert(key, receiver);
        SubmitSlot::Leader(InFlightGuard {
            cache: Arc::clone(self),
            key,
            sender,
            settled: false,
        })
    }

    fn contains_at(&self, key: HashKey, now: Instant) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        matches!(state.entries.get(&key), Some(at) if now.duration_since(*at) < self.ttl)
    }

    fn record_accepted_at(&self, key: HashKey, now: Instant) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let state = &mut *state;

        // Shed expired rows, then make room. The log is in arrival order
        // and the TTL is constant, so the front is always oldest.
        while let Some((front_key, front_at)) = state.insertion_log.front().copied() {
            let expired = now.duration_since(front_at) >= self.ttl;
            let over_capacity = state.insertion_log.len() >= self.max_entries;
            if !expired && !over_capacity {
                break;
            }
            state.insertion_log.pop_front();
            if let Entry::Occupied(entry) = state.entries.entry(front_key)
                && *entry.get() == front_at
            {
                entry.remove();
            }
        }

        state.entries.insert(key, now);
        state.insertion_log.push_back((key, now));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }
}

fn key(hash: &TxHash) -> HashKey {
    let mut key = HashKey::default();
    key.copy_from_slice(hash.0.0.as_ref());
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_key(x: u8) -> HashKey {
        [x; 32]
    }

    #[test]
    fn accepted_hash_is_deduplicated_until_the_ttl_expires() {
        let cache = AcceptedTxCache::new(10, Duration::from_mins(1));
        let start = Instant::now();

        assert!(!cache.contains_at(hash_key(1), start));
        cache.record_accepted_at(hash_key(1), start);
        assert!(cache.contains_at(hash_key(1), start + Duration::from_secs(59)));
        assert!(!cache.contains_at(hash_key(1), start + Duration::from_mins(1)));
    }

    #[test]
    fn capacity_evicts_oldest_entries_first() {
        let cache = AcceptedTxCache::new(2, Duration::from_mins(1));
        let start = Instant::now();

        cache.record_accepted_at(hash_key(1), start);
        cache.record_accepted_at(hash_key(2), start + Duration::from_secs(1));
        cache.record_accepted_at(hash_key(3), start + Duration::from_secs(2));

        let now = start + Duration::from_secs(3);
        assert!(!cache.contains_at(hash_key(1), now), "oldest entry evicted");
        assert!(cache.contains_at(hash_key(2), now));
        assert!(cache.contains_at(hash_key(3), now));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn expired_entries_are_shed_on_insert() {
        let cache = AcceptedTxCache::new(10, Duration::from_secs(10));
        let start = Instant::now();

        cache.record_accepted_at(hash_key(1), start);
        cache.record_accepted_at(hash_key(2), start + Duration::from_secs(20));

        assert_eq!(cache.len(), 1);
        assert!(cache.contains_at(hash_key(2), start + Duration::from_secs(21)));
    }

    #[test]
    fn reinserting_a_hash_refreshes_its_window() {
        let cache = AcceptedTxCache::new(3, Duration::from_secs(10));
        let start = Instant::now();

        cache.record_accepted_at(hash_key(1), start);
        cache.record_accepted_at(hash_key(1), start + Duration::from_secs(5));
        // Filling the cache evicts the stale first log row for hash 1;
        // its refreshed entry must survive that eviction.
        cache.record_accepted_at(hash_key(2), start + Duration::from_secs(6));
        cache.record_accepted_at(hash_key(3), start + Duration::from_secs(7));

        assert!(cache.contains_at(hash_key(1), start + Duration::from_secs(14)));
        assert!(cache.contains_at(hash_key(2), start + Duration::from_secs(14)));
        assert!(cache.contains_at(hash_key(3), start + Duration::from_secs(14)));
    }

    fn tx_hash(x: u8) -> TxHash {
        TxHash(groundhog_json_rpc_types::hex::Hex0x(
            groundhog_xdr::types::SHA256Hash::from([x; 32]),
        ))
    }

    #[test]
    fn concurrent_claims_get_one_leader_and_waiters() {
        let cache = Arc::new(AcceptedTxCache::new(10, Duration::from_mins(1)));

        let SubmitSlot::Leader(guard) = cache.begin_submit(&tx_hash(1)) else {
            panic!("first claim should lead");
        };
        let SubmitSlot::Waiter(receiver) = cache.begin_submit(&tx_hash(1)) else {
            panic!("second concurrent claim should wait");
        };
        assert_eq!(*receiver.borrow(), InFlightOutcome::Pending);

        guard.settle_accepted();
        assert_eq!(*receiver.borrow(), InFlightOutcome::Accepted);
        assert!(matches!(
            cache.begin_submit(&tx_hash(1)),
            SubmitSlot::AlreadyAccepted
        ));
    }

    #[test]
    fn dropped_leader_releases_waiters_as_failed_and_frees_the_slot() {
        let cache = Arc::new(AcceptedTxCache::new(10, Duration::from_mins(1)));

        let SubmitSlot::Leader(guard) = cache.begin_submit(&tx_hash(1)) else {
            panic!("first claim should lead");
        };
        let SubmitSlot::Waiter(receiver) = cache.begin_submit(&tx_hash(1)) else {
            panic!("second concurrent claim should wait");
        };

        drop(guard);
        assert_eq!(*receiver.borrow(), InFlightOutcome::Failed);
        assert!(
            matches!(cache.begin_submit(&tx_hash(1)), SubmitSlot::Leader(_)),
            "failed hash should be claimable again"
        );
        assert!(!cache.contains(&tx_hash(1)));
    }

    #[test]
    fn from_config_respects_disablement_and_empty_bounds() {
        assert!(AcceptedTxCache::from_config(&SubmitDedupeConfig::default()).is_some());
        assert!(
            AcceptedTxCache::from_config(&SubmitDedupeConfig {
                enabled: false,
                ..SubmitDedupeConfig::default()
            })
            .is_none()
        );
        assert!(
            AcceptedTxCache::from_config(&SubmitDedupeConfig {
                max_entries: 0,
                ..SubmitDedupeConfig::default()
            })
            .is_none()
        );
        assert!(
            AcceptedTxCache::from_config(&SubmitDedupeConfig {
                ttl_ms: 0,
                ..SubmitDedupeConfig::default()
            })
            .is_none()
        );
    }
}
