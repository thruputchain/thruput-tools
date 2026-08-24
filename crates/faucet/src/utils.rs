use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Return a transaction nonce that is unique within this process.
///
/// Every faucet transaction is signed by the same faucet wallet, so the
/// `(source, nonce)` pair is the chain's duplicate key: two transactions that
/// reuse a nonce collide, and the second is rejected. A raw timestamp is not
/// unique under concurrent requests, so this hands out values from a
/// monotonic counter seeded once from the wall clock — the seed keeps nonces
/// from repeating across process restarts, and the counter keeps them unique
/// within one.
pub(crate) fn next_nonce() -> u64 {
    static NEXT_NONCE: OnceLock<AtomicU64> = OnceLock::new();
    NEXT_NONCE
        .get_or_init(|| AtomicU64::new(nonce_seed()))
        .fetch_add(1, Ordering::Relaxed)
}

/// Nanoseconds since the Unix epoch, saturating instead of failing: a clock
/// before the epoch (or one overflowing u64 nanoseconds, ~584 years in) still
/// yields a usable starting point for the counter.
fn nonce_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::next_nonce;
    use std::collections::HashSet;

    #[test]
    fn concurrent_nonces_are_unique() {
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| (0..1000).map(|_| next_nonce()).collect::<Vec<_>>()))
            .collect();

        let mut seen = HashSet::new();
        for handle in handles {
            for nonce in handle.join().expect("bug: nonce thread panicked") {
                assert!(seen.insert(nonce), "duplicate nonce {nonce}");
            }
        }
    }

    #[test]
    fn nonces_increase_monotonically() {
        let first = next_nonce();
        let second = next_nonce();
        assert!(second > first);
    }
}
