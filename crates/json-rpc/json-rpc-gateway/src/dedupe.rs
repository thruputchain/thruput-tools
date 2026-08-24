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
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use groundhog_json_rpc_types::transaction::TxHash;

use crate::config::SubmitDedupeConfig;

type HashKey = [u8; 32];

/// Bounded, TTL'd set of recently accepted transaction hashes.
pub(crate) struct AcceptedTxCache {
    ttl: Duration,
    max_entries: usize,
    state: Mutex<CacheState>,
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
        }
    }

    /// Whether `hash` was accepted within the TTL window.
    pub(crate) fn contains(&self, hash: &TxHash) -> bool {
        self.contains_at(key(hash), Instant::now())
    }

    fn contains_at(&self, key: HashKey, now: Instant) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        matches!(state.entries.get(&key), Some(at) if now.duration_since(*at) < self.ttl)
    }

    /// Record `hash` as accepted by the downstream mempool path.
    pub(crate) fn record_accepted(&self, hash: &TxHash) {
        self.record_accepted_at(key(hash), Instant::now());
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
