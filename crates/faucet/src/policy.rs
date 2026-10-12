use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use axum::http::HeaderMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AirdropAttemptKey {
    pub network_id: String,
    pub genesis_hash: String,
    pub recipient: String,
    pub client_nonce: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirdropAttemptStatus {
    Pending,
    Executing,
    Ambiguous,
    Succeeded,
    FailedRetryable,
    FailedTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirdropAttempt {
    pub key: AirdropAttemptKey,
    pub client_identity: ClientIdentity,
    pub raw_amount: String,
    pub status: AirdropAttemptStatus,
    pub execution_id: u64,
    pub tx_hash: Option<String>,
    /// Signed evidence retained before broadcast; absent in older/custom stores.
    pub transaction: Option<crate::airdrop::PreparedAirdrop>,
    pub height: Option<i64>,
    pub error_code: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitAirdropAttemptResult {
    Inserted,
    Existing(Box<AirdropAttempt>),
    CapacityReached,
    PendingLimitReached,
    QuotaExceeded(QuotaDecision),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaCheck {
    pub key: QuotaKey,
    pub max: u32,
    pub window: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuotaKey {
    AirdropRecipient(String),
    AirdropClient(ClientIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaDecision {
    pub allowed: bool,
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientIdentity {
    Ip(IpAddr),
    Unknown,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum PolicyStoreError {
    #[error("policy store lock was poisoned")]
    LockPoisoned,
    #[error("attempt was not found")]
    AttemptNotFound,
    #[error("attempt transition expected one of {expected:?}, got {current:?}")]
    InvalidAirdropTransition {
        expected: Vec<AirdropAttemptStatus>,
        current: AirdropAttemptStatus,
    },
    #[error("quota window cannot be represented by the system clock")]
    QuotaWindowTooLarge,
    #[error("airdrop execution ID space is exhausted")]
    AirdropExecutionIdExhausted,
}

pub trait FaucetPolicyStore: Send + Sync + 'static {
    fn get_airdrop_attempt(
        &self,
        key: &AirdropAttemptKey,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError>;

    fn put_airdrop_attempt(
        &self,
        attempt: AirdropAttempt,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError>;

    fn admit_airdrop_attempt(
        &self,
        attempt: AirdropAttempt,
        attempt_capacity: usize,
        max_pending_per_recipient: u32,
        quota_checks: &[QuotaCheck],
        now: SystemTime,
    ) -> Result<AdmitAirdropAttemptResult, PolicyStoreError>;

    fn claim_airdrop_transition(
        &self,
        key: &AirdropAttemptKey,
        expected: &[AirdropAttemptStatus],
        expected_execution_id: Option<u64>,
        next: AirdropAttemptStatus,
        now: SystemTime,
    ) -> Result<AirdropAttempt, PolicyStoreError>;

    fn replace_airdrop_attempt_if_status(
        &self,
        expected: &[AirdropAttemptStatus],
        attempt: AirdropAttempt,
    ) -> Result<AirdropAttempt, PolicyStoreError>;

    fn check_and_record_quota(
        &self,
        key: QuotaKey,
        max: u32,
        window: Duration,
        now: SystemTime,
    ) -> Result<QuotaDecision, PolicyStoreError>;

    fn check_and_record_quotas(
        &self,
        checks: &[QuotaCheck],
        now: SystemTime,
    ) -> Result<QuotaDecision, PolicyStoreError>;

    fn expire(&self, now: SystemTime) -> Result<(), PolicyStoreError>;
}

#[derive(Debug, Default)]
pub struct InMemoryFaucetPolicyStore {
    state: Mutex<PolicyState>,
}

#[derive(Debug, Default)]
struct PolicyState {
    airdrop_attempts: HashMap<AirdropAttemptKey, AirdropAttempt>,
    quota_events: HashMap<QuotaKey, Vec<QuotaEvent>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuotaEvent {
    occurred_at: SystemTime,
    expires_at: SystemTime,
}

impl InMemoryFaucetPolicyStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, PolicyState>, PolicyStoreError> {
        self.state
            .lock()
            .map_err(|_| PolicyStoreError::LockPoisoned)
    }
}

impl FaucetPolicyStore for InMemoryFaucetPolicyStore {
    fn get_airdrop_attempt(
        &self,
        key: &AirdropAttemptKey,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError> {
        Ok(self.lock()?.airdrop_attempts.get(key).cloned())
    }

    fn put_airdrop_attempt(
        &self,
        attempt: AirdropAttempt,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError> {
        Ok(self
            .lock()?
            .airdrop_attempts
            .insert(attempt.key.clone(), attempt))
    }

    fn admit_airdrop_attempt(
        &self,
        attempt: AirdropAttempt,
        attempt_capacity: usize,
        max_pending_per_recipient: u32,
        quota_checks: &[QuotaCheck],
        now: SystemTime,
    ) -> Result<AdmitAirdropAttemptResult, PolicyStoreError> {
        let quota_expiries = quota_checks
            .iter()
            .map(|check| {
                now.checked_add(check.window)
                    .ok_or(PolicyStoreError::QuotaWindowTooLarge)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut state = self.lock()?;
        if let Some(existing) = state.airdrop_attempts.get(&attempt.key) {
            return Ok(AdmitAirdropAttemptResult::Existing(Box::new(
                existing.clone(),
            )));
        }
        if state.airdrop_attempts.len() >= attempt_capacity {
            return Ok(AdmitAirdropAttemptResult::CapacityReached);
        }
        let pending = state
            .airdrop_attempts
            .values()
            .filter(|existing| {
                existing.key.network_id == attempt.key.network_id
                    && existing.key.genesis_hash == attempt.key.genesis_hash
                    && existing.key.recipient == attempt.key.recipient
                    && matches!(
                        existing.status,
                        AirdropAttemptStatus::Pending
                            | AirdropAttemptStatus::Executing
                            | AirdropAttemptStatus::Ambiguous
                    )
            })
            .count();
        if pending >= max_pending_per_recipient as usize {
            return Ok(AdmitAirdropAttemptResult::PendingLimitReached);
        }

        for check in quota_checks {
            let events = state.quota_events.entry(check.key.clone()).or_default();
            events.retain(|event| event.expires_at > now);
            if events.len() >= check.max as usize {
                let retry_after = events
                    .first()
                    .and_then(|event| event.expires_at.duration_since(now).ok());
                return Ok(AdmitAirdropAttemptResult::QuotaExceeded(QuotaDecision {
                    allowed: false,
                    retry_after,
                }));
            }
        }
        for (check, expires_at) in quota_checks.iter().zip(quota_expiries) {
            state
                .quota_events
                .entry(check.key.clone())
                .or_default()
                .push(QuotaEvent {
                    occurred_at: now,
                    expires_at,
                });
        }
        state.airdrop_attempts.insert(attempt.key.clone(), attempt);
        Ok(AdmitAirdropAttemptResult::Inserted)
    }

    fn claim_airdrop_transition(
        &self,
        key: &AirdropAttemptKey,
        expected: &[AirdropAttemptStatus],
        expected_execution_id: Option<u64>,
        next: AirdropAttemptStatus,
        now: SystemTime,
    ) -> Result<AirdropAttempt, PolicyStoreError> {
        let mut state = self.lock()?;
        let attempt = state
            .airdrop_attempts
            .get_mut(key)
            .ok_or(PolicyStoreError::AttemptNotFound)?;
        if !expected.contains(&attempt.status)
            || expected_execution_id.is_some_and(|expected| expected != attempt.execution_id)
        {
            return Err(PolicyStoreError::InvalidAirdropTransition {
                expected: expected.to_vec(),
                current: attempt.status,
            });
        }
        if next == AirdropAttemptStatus::Executing {
            attempt.execution_id = attempt
                .execution_id
                .checked_add(1)
                .ok_or(PolicyStoreError::AirdropExecutionIdExhausted)?;
        }
        attempt.status = next;
        attempt.updated_at = now;
        Ok(attempt.clone())
    }

    fn replace_airdrop_attempt_if_status(
        &self,
        expected: &[AirdropAttemptStatus],
        attempt: AirdropAttempt,
    ) -> Result<AirdropAttempt, PolicyStoreError> {
        let mut state = self.lock()?;
        let current = state
            .airdrop_attempts
            .get_mut(&attempt.key)
            .ok_or(PolicyStoreError::AttemptNotFound)?;
        if !expected.contains(&current.status) || current.execution_id != attempt.execution_id {
            return Err(PolicyStoreError::InvalidAirdropTransition {
                expected: expected.to_vec(),
                current: current.status,
            });
        }
        *current = attempt;
        Ok(current.clone())
    }

    fn check_and_record_quota(
        &self,
        key: QuotaKey,
        max: u32,
        window: Duration,
        now: SystemTime,
    ) -> Result<QuotaDecision, PolicyStoreError> {
        let expires_at = now
            .checked_add(window)
            .ok_or(PolicyStoreError::QuotaWindowTooLarge)?;
        let mut state = self.lock()?;
        let events = state.quota_events.entry(key).or_default();
        events.retain(|event| event.expires_at > now);
        if events.len() >= max as usize {
            let retry_after = events
                .first()
                .and_then(|event| event.expires_at.duration_since(now).ok());
            return Ok(QuotaDecision {
                allowed: false,
                retry_after,
            });
        }
        events.push(QuotaEvent {
            occurred_at: now,
            expires_at,
        });
        Ok(QuotaDecision {
            allowed: true,
            retry_after: None,
        })
    }

    fn check_and_record_quotas(
        &self,
        checks: &[QuotaCheck],
        now: SystemTime,
    ) -> Result<QuotaDecision, PolicyStoreError> {
        let quota_expiries = checks
            .iter()
            .map(|check| {
                now.checked_add(check.window)
                    .ok_or(PolicyStoreError::QuotaWindowTooLarge)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut state = self.lock()?;
        for check in checks {
            let events = state.quota_events.entry(check.key.clone()).or_default();
            events.retain(|event| event.expires_at > now);
            if events.len() >= check.max as usize {
                let retry_after = events
                    .first()
                    .and_then(|event| event.expires_at.duration_since(now).ok());
                return Ok(QuotaDecision {
                    allowed: false,
                    retry_after,
                });
            }
        }
        for (check, expires_at) in checks.iter().zip(quota_expiries) {
            state
                .quota_events
                .entry(check.key.clone())
                .or_default()
                .push(QuotaEvent {
                    occurred_at: now,
                    expires_at,
                });
        }
        Ok(QuotaDecision {
            allowed: true,
            retry_after: None,
        })
    }

    fn expire(&self, now: SystemTime) -> Result<(), PolicyStoreError> {
        let mut state = self.lock()?;
        state.airdrop_attempts.retain(|_, attempt| {
            matches!(
                attempt.status,
                AirdropAttemptStatus::Pending
                    | AirdropAttemptStatus::Executing
                    | AirdropAttemptStatus::Ambiguous
            ) || attempt.expires_at.is_none_or(|expires_at| expires_at > now)
        });
        for events in state.quota_events.values_mut() {
            events.retain(|event| event.expires_at > now);
        }
        state.quota_events.retain(|_, events| !events.is_empty());
        Ok(())
    }
}

pub fn client_identity(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    trust_proxy_client_ip_headers: bool,
) -> ClientIdentity {
    if trust_proxy_client_ip_headers && let Some(ip) = forwarded_client_ip(headers) {
        return ClientIdentity::Ip(ip);
    }
    peer.map_or(ClientIdentity::Unknown, |addr| {
        ClientIdentity::Ip(addr.ip())
    })
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .and_then(|value| value.trim().parse().ok())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse().ok())
        })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn airdrop_transition_claim_updates_status_atomically() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let key = airdrop_key();
        store
            .put_airdrop_attempt(AirdropAttempt {
                key: key.clone(),
                client_identity: ClientIdentity::Unknown,
                raw_amount: "100000000".into(),
                status: AirdropAttemptStatus::Pending,
                execution_id: 0,
                tx_hash: None,
                transaction: None,
                height: None,
                error_code: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
            })
            .expect("bug: insert should succeed");

        let claimed = store
            .claim_airdrop_transition(
                &key,
                &[AirdropAttemptStatus::Pending],
                None,
                AirdropAttemptStatus::Succeeded,
                now + Duration::from_secs(1),
            )
            .expect("bug: transition should be valid");

        assert_eq!(claimed.status, AirdropAttemptStatus::Succeeded);
    }

    #[test]
    fn conditional_airdrop_replacement_commits_one_coherent_terminal_state() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let key = airdrop_key();
        let executing = AirdropAttempt {
            key: key.clone(),
            client_identity: ClientIdentity::Unknown,
            raw_amount: "100000000".into(),
            status: AirdropAttemptStatus::Executing,
            execution_id: 1,
            tx_hash: Some("0x01".into()),
            transaction: None,
            height: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
        };
        store
            .put_airdrop_attempt(executing.clone())
            .expect("bug: insert should succeed");
        let mut succeeded = executing.clone();
        succeeded.status = AirdropAttemptStatus::Succeeded;
        succeeded.height = Some(42);
        store
            .replace_airdrop_attempt_if_status(
                &[AirdropAttemptStatus::Executing],
                succeeded.clone(),
            )
            .expect("bug: executing attempt should commit once");
        let mut stale_failure = executing;
        stale_failure.status = AirdropAttemptStatus::FailedTerminal;
        stale_failure.error_code = Some("stale".into());

        let error = store
            .replace_airdrop_attempt_if_status(&[AirdropAttemptStatus::Executing], stale_failure)
            .expect_err("bug: stale worker must not overwrite terminal state");

        assert!(matches!(
            error,
            PolicyStoreError::InvalidAirdropTransition { .. }
        ));
        assert_eq!(
            store
                .get_airdrop_attempt(&key)
                .expect("bug: lookup should succeed"),
            Some(succeeded)
        );
    }

    #[test]
    fn stale_airdrop_worker_cannot_overwrite_a_new_execution_claim() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let attempt = airdrop_attempt("0xnonce");
        let key = attempt.key.clone();
        store
            .put_airdrop_attempt(attempt)
            .expect("bug: insert should succeed");
        let old_claim = store
            .claim_airdrop_transition(
                &key,
                &[AirdropAttemptStatus::Pending],
                None,
                AirdropAttemptStatus::Executing,
                now,
            )
            .expect("bug: first worker should claim the attempt");
        store
            .claim_airdrop_transition(
                &key,
                &[AirdropAttemptStatus::Executing],
                Some(old_claim.execution_id),
                AirdropAttemptStatus::Pending,
                now,
            )
            .expect("bug: first worker should release its claim");
        let new_claim = store
            .claim_airdrop_transition(
                &key,
                &[AirdropAttemptStatus::Pending],
                None,
                AirdropAttemptStatus::Executing,
                now,
            )
            .expect("bug: second worker should claim the attempt");
        let release_error = store
            .claim_airdrop_transition(
                &key,
                &[AirdropAttemptStatus::Executing],
                Some(old_claim.execution_id),
                AirdropAttemptStatus::Pending,
                now,
            )
            .expect_err("bug: stale worker must not release a newer execution claim");
        assert!(matches!(
            release_error,
            PolicyStoreError::InvalidAirdropTransition { .. }
        ));
        let mut stale_completion = old_claim;
        stale_completion.status = AirdropAttemptStatus::Succeeded;

        let error = store
            .replace_airdrop_attempt_if_status(&[AirdropAttemptStatus::Executing], stale_completion)
            .expect_err("bug: stale worker must not overwrite a newer execution claim");

        assert!(matches!(
            error,
            PolicyStoreError::InvalidAirdropTransition { .. }
        ));
        assert_eq!(
            store
                .get_airdrop_attempt(&key)
                .expect("bug: lookup should succeed"),
            Some(new_claim)
        );
    }

    #[test]
    fn quota_rejects_after_limit_until_window_expires() {
        let store = InMemoryFaucetPolicyStore::new();
        let key = QuotaKey::AirdropRecipient("0xabc".into());
        let now = SystemTime::UNIX_EPOCH;
        let window = Duration::from_mins(1);

        assert!(
            store
                .check_and_record_quota(key.clone(), 1, window, now)
                .expect("bug: quota check should succeed")
                .allowed
        );
        let blocked = store
            .check_and_record_quota(key.clone(), 1, window, now + Duration::from_secs(1))
            .expect("bug: quota check should succeed");
        assert!(!blocked.allowed);
        assert_eq!(blocked.retry_after, Some(Duration::from_secs(59)));
        assert!(
            store
                .check_and_record_quota(key, 1, window, now + Duration::from_secs(61))
                .expect("bug: quota check should succeed")
                .allowed
        );
    }

    #[test]
    fn quota_window_overflow_fails_closed_without_recording_an_event() {
        let store = InMemoryFaucetPolicyStore::new();
        let key = QuotaKey::AirdropRecipient("0xabc".into());

        let error = store
            .check_and_record_quota(key.clone(), 1, Duration::MAX, SystemTime::now())
            .expect_err("unrepresentable quota window must fail closed");

        assert!(matches!(error, PolicyStoreError::QuotaWindowTooLarge));
        assert!(
            !store
                .lock()
                .expect("bug: store lock should succeed")
                .quota_events
                .contains_key(&key)
        );
    }

    #[test]
    fn quota_batch_does_not_record_when_any_check_fails() {
        let store = InMemoryFaucetPolicyStore::new();
        let first = QuotaKey::AirdropRecipient("0xabc".into());
        let second = QuotaKey::AirdropClient(ClientIdentity::Unknown);
        let now = SystemTime::UNIX_EPOCH;
        let window = Duration::from_mins(1);

        let blocked = store
            .check_and_record_quotas(
                &[
                    QuotaCheck {
                        key: first.clone(),
                        max: 1,
                        window,
                    },
                    QuotaCheck {
                        key: second,
                        max: 0,
                        window,
                    },
                ],
                now,
            )
            .expect("bug: quota check should succeed");

        assert!(!blocked.allowed);
        assert!(
            store
                .check_and_record_quota(first, 1, window, now)
                .expect("bug: quota check should succeed")
                .allowed
        );
    }

    #[test]
    fn concurrent_same_key_admission_records_one_quota_event() {
        let store = Arc::new(InMemoryFaucetPolicyStore::new());
        let barrier = Arc::new(Barrier::new(3));
        let now = SystemTime::UNIX_EPOCH;
        let checks = vec![QuotaCheck {
            key: QuotaKey::AirdropClient(ClientIdentity::Unknown),
            max: 2,
            window: Duration::from_mins(1),
        }];
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                let checks = checks.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .admit_airdrop_attempt(airdrop_attempt("0xnonce"), 10, 1, &checks, now)
                        .expect("bug: admission should succeed")
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("bug: admission thread should join"))
            .collect();

        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, AdmitAirdropAttemptResult::Inserted))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, AdmitAirdropAttemptResult::Existing(_)))
                .count(),
            1
        );
        assert!(matches!(
            store
                .admit_airdrop_attempt(airdrop_attempt("0xsecond"), 10, 1, &checks, now)
                .expect("bug: second admission should succeed"),
            AdmitAirdropAttemptResult::Inserted
        ));
        assert!(matches!(
            store
                .admit_airdrop_attempt(airdrop_attempt("0xthird"), 10, 1, &checks, now)
                .expect("bug: quota rejection should succeed"),
            AdmitAirdropAttemptResult::QuotaExceeded(_)
        ));
    }

    // Explicit qualification: cargo test -p thruput-faucet retained_capacity_load -- --ignored --nocapture
    #[test]
    #[ignore = "qualifies policy scans at hosted retention occupancy"]
    fn retained_capacity_load() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_hours(12);
        // Populate the previous 12 hours at 10k/hour with real-sized identities.
        for index in 0..120_000 {
            let mut attempt = airdrop_attempt(&format!("{index:064x}"));
            attempt.key.network_id = "ab".repeat(32);
            attempt.key.genesis_hash = "cd".repeat(32);
            attempt.key.recipient = format!("0x{index:064x}");
            attempt.status = AirdropAttemptStatus::Succeeded;
            attempt.tx_hash = Some(format!("0x{index:064x}"));
            attempt.expires_at = Some(now + Duration::from_hours(12));
            store.put_airdrop_attempt(attempt).unwrap();
        }
        let started = std::time::Instant::now();
        for index in 0..1_000 {
            let at = now + Duration::from_secs(index * 3600 / 10_000);
            let attempt = airdrop_attempt(&format!("new-{index:064x}"));
            let checks = [
                QuotaCheck {
                    key: QuotaKey::AirdropClient(ClientIdentity::Unknown),
                    max: 10_000,
                    window: Duration::from_hours(1),
                },
                QuotaCheck {
                    key: QuotaKey::AirdropRecipient(attempt.key.recipient.clone()),
                    max: 1,
                    window: Duration::from_mins(1),
                },
            ];
            store.expire(at).unwrap();
            assert!(matches!(
                store
                    .admit_airdrop_attempt(attempt.clone(), 200_000, 1, &checks, at)
                    .unwrap(),
                AdmitAirdropAttemptResult::Inserted
            ));
            assert!(matches!(
                store
                    .admit_airdrop_attempt(attempt, 200_000, 1, &checks, at)
                    .unwrap(),
                AdmitAirdropAttemptResult::Existing(_)
            ));
        }
        eprintln!(
            "1000 admissions plus retries at 120k retained records: {:?}",
            started.elapsed()
        );
        assert_eq!(store.lock().unwrap().airdrop_attempts.len(), 121_000);
        store.expire(now + Duration::from_hours(12)).unwrap();
        // Terminal records expire; unresolved records survive, even beyond retention.
        assert_eq!(store.lock().unwrap().airdrop_attempts.len(), 1_000);
        assert!(store.lock().unwrap().quota_events.is_empty());
        // Restart remains explicitly in-memory: no persistence is added by capacity sizing.
        drop(store);
        assert!(
            InMemoryFaucetPolicyStore::new()
                .lock()
                .unwrap()
                .airdrop_attempts
                .is_empty()
        );
    }

    #[test]
    fn airdrop_attempt_capacity_fails_closed_for_unresolved_transaction() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let checks = [QuotaCheck {
            key: QuotaKey::AirdropClient(ClientIdentity::Unknown),
            max: 10,
            window: Duration::from_mins(1),
        }];
        let mut first = airdrop_attempt("0xfirst");
        first.tx_hash = Some("0xaccepted".into());

        assert!(matches!(
            store
                .admit_airdrop_attempt(first.clone(), 1, 10, &checks, now)
                .expect("bug: first admission should succeed"),
            AdmitAirdropAttemptResult::Inserted
        ));
        assert!(matches!(
            store
                .admit_airdrop_attempt(first, 1, 10, &checks, now)
                .expect("bug: retry lookup should succeed"),
            AdmitAirdropAttemptResult::Existing(_)
        ));
        assert!(matches!(
            store
                .admit_airdrop_attempt(airdrop_attempt("0xsecond"), 1, 10, &checks, now)
                .expect("bug: capacity rejection should succeed"),
            AdmitAirdropAttemptResult::CapacityReached
        ));
    }

    #[test]
    fn expire_removes_only_attempts_with_elapsed_retention_and_quota_events() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let airdrop_key = airdrop_key();
        let stale_airdrop_key = AirdropAttemptKey {
            client_nonce: "0xstale".into(),
            ..airdrop_key.clone()
        };
        store
            .put_airdrop_attempt(AirdropAttempt {
                key: airdrop_key.clone(),
                client_identity: ClientIdentity::Unknown,
                raw_amount: "100000000".into(),
                status: AirdropAttemptStatus::FailedTerminal,
                execution_id: 0,
                tx_hash: None,
                transaction: None,
                height: None,
                error_code: Some("boom".into()),
                created_at: now,
                updated_at: now,
                expires_at: None,
            })
            .expect("bug: insert should succeed");
        store
            .put_airdrop_attempt(AirdropAttempt {
                key: stale_airdrop_key.clone(),
                client_identity: ClientIdentity::Unknown,
                raw_amount: "100000000".into(),
                status: AirdropAttemptStatus::FailedRetryable,
                execution_id: 0,
                tx_hash: None,
                transaction: None,
                height: None,
                error_code: None,
                created_at: now,
                updated_at: now,
                expires_at: Some(now + Duration::from_secs(5)),
            })
            .expect("bug: insert should succeed");
        store
            .check_and_record_quota(
                QuotaKey::AirdropRecipient("0xabc".into()),
                1,
                Duration::from_secs(5),
                now,
            )
            .expect("bug: quota check should succeed");

        store
            .expire(now + Duration::from_secs(6))
            .expect("bug: expire should succeed");

        assert!(
            store
                .get_airdrop_attempt(&airdrop_key)
                .expect("bug: lookup should succeed")
                .is_some()
        );
        assert!(
            store
                .get_airdrop_attempt(&stale_airdrop_key)
                .expect("bug: lookup should succeed")
                .is_none()
        );
        assert!(
            store
                .check_and_record_quota(
                    QuotaKey::AirdropRecipient("0xabc".into()),
                    1,
                    Duration::from_secs(5),
                    now + Duration::from_secs(6),
                )
                .expect("bug: quota check should succeed")
                .allowed
        );
    }

    #[test]
    fn client_identity_ignores_forwarded_headers_unless_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.10".parse().unwrap());
        let peer = SocketAddr::from((Ipv4Addr::LOCALHOST, 8081));

        assert_eq!(
            client_identity(&headers, Some(peer), false),
            ClientIdentity::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
        assert_eq!(
            client_identity(&headers, Some(peer), true),
            ClientIdentity::Ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)))
        );
    }

    fn airdrop_key() -> AirdropAttemptKey {
        AirdropAttemptKey {
            network_id: "devnet-preview".into(),
            genesis_hash: "0xgenesis".into(),
            recipient: "0xrecipient".into(),
            client_nonce: "0xnonce".into(),
        }
    }

    fn airdrop_attempt(client_nonce: &str) -> AirdropAttempt {
        let now = SystemTime::UNIX_EPOCH;
        AirdropAttempt {
            key: AirdropAttemptKey {
                client_nonce: client_nonce.into(),
                recipient: format!("0xrecipient-{client_nonce}"),
                ..airdrop_key()
            },
            client_identity: ClientIdentity::Unknown,
            raw_amount: "100000000".into(),
            status: AirdropAttemptStatus::Pending,
            execution_id: 0,
            tx_hash: None,
            transaction: None,
            height: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
        }
    }
}
