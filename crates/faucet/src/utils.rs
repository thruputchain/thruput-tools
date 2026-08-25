use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Return a transaction nonce that is unique within this process.
///
/// The replay cache keys transactions by invoked address and the complete
/// `(nonce, block_expiry_height)` uniquifier. Concurrent faucet requests often
/// share an expiry height, so a raw timestamp does not reliably distinguish
/// them. This counter guarantees distinct nonces within one process. Its clock
/// seed makes overlap across normal restarts unlikely, but does not guarantee
/// restart or multi-process uniqueness.
pub(crate) fn next_nonce() -> Result<u64, NonceExhausted> {
    static NEXT_NONCE: OnceLock<AtomicU64> = OnceLock::new();
    take_nonce(NEXT_NONCE.get_or_init(|| AtomicU64::new(nonce_seed())))
}

/// Indicates that no additional transaction nonce can be allocated.
#[derive(Debug, thiserror::Error)]
#[error("transaction nonce space exhausted")]
pub(crate) struct NonceExhausted;

fn take_nonce(counter: &AtomicU64) -> Result<u64, NonceExhausted> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |nonce| {
            nonce.checked_add(1)
        })
        .map_err(|_| NonceExhausted)
}

/// Nanoseconds since the Unix epoch. A clock before the epoch starts at 1;
/// an overflowing timestamp fails closed through the exhausted counter.
fn nonce_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{next_nonce, take_nonce};

    #[test]
    fn concurrent_nonces_are_unique() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..1000)
                        .map(|_| next_nonce().expect("bug: nonce space should be available"))
                        .collect::<Vec<_>>()
                })
            })
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
        let first = next_nonce().expect("bug: nonce space should be available");
        let second = next_nonce().expect("bug: nonce space should be available");
        assert!(second > first);
    }

    #[test]
    fn nonce_exhaustion_does_not_wrap() {
        let counter = AtomicU64::new(u64::MAX);

        assert!(take_nonce(&counter).is_err());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
