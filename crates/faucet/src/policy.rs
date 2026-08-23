use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use axum::http::HeaderMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WalletCreateAttemptKey {
    pub network_id: String,
    pub genesis_hash: String,
    pub owner_public_key: String,
    pub client_nonce: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletCreateAttemptStatus {
    Prepared,
    PrefundPending,
    PrefundConfirmed,
    CreatePending,
    Created,
    FailedRetryable,
    FailedTerminal,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletCreateAttempt {
    pub key: WalletCreateAttemptKey,
    pub deploy_nonce: String,
    pub wallet_address: String,
    pub wallet_contract_hash: String,
    pub prefund_raw_amount: String,
    pub status: WalletCreateAttemptStatus,
    pub prefund_tx_hash: Option<String>,
    pub prefund_height: Option<i64>,
    pub create_tx_hash: Option<String>,
    pub create_height: Option<i64>,
    pub error_code: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub expires_at: Option<SystemTime>,
}

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
    pub tx_hash: Option<String>,
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
    PendingLimitReached,
    QuotaExceeded(QuotaDecision),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertWalletCreateAttemptResult {
    Inserted,
    Existing(Box<WalletCreateAttempt>),
    PendingLimitReached,
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
    WalletCreateOwner(String),
    WalletCreateClient(ClientIdentity),
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
    InvalidWalletCreateTransition {
        expected: Vec<WalletCreateAttemptStatus>,
        current: WalletCreateAttemptStatus,
    },
    #[error("attempt transition expected one of {expected:?}, got {current:?}")]
    InvalidAirdropTransition {
        expected: Vec<AirdropAttemptStatus>,
        current: AirdropAttemptStatus,
    },
}

pub trait FaucetPolicyStore: Send + Sync + 'static {
    fn get_wallet_create_attempt(
        &self,
        key: &WalletCreateAttemptKey,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError>;

    fn put_wallet_create_attempt(
        &self,
        attempt: WalletCreateAttempt,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError>;

    fn insert_wallet_create_attempt_if_absent(
        &self,
        attempt: WalletCreateAttempt,
        max_pending_per_owner: u32,
    ) -> Result<InsertWalletCreateAttemptResult, PolicyStoreError>;

    fn claim_wallet_create_transition(
        &self,
        key: &WalletCreateAttemptKey,
        expected: &[WalletCreateAttemptStatus],
        next: WalletCreateAttemptStatus,
        now: SystemTime,
    ) -> Result<WalletCreateAttempt, PolicyStoreError>;

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
        max_pending_per_recipient: u32,
        quota_checks: &[QuotaCheck],
        now: SystemTime,
    ) -> Result<AdmitAirdropAttemptResult, PolicyStoreError>;

    fn claim_airdrop_transition(
        &self,
        key: &AirdropAttemptKey,
        expected: &[AirdropAttemptStatus],
        next: AirdropAttemptStatus,
        now: SystemTime,
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
    wallet_create_attempts: HashMap<WalletCreateAttemptKey, WalletCreateAttempt>,
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
    fn get_wallet_create_attempt(
        &self,
        key: &WalletCreateAttemptKey,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError> {
        Ok(self.lock()?.wallet_create_attempts.get(key).cloned())
    }

    fn put_wallet_create_attempt(
        &self,
        attempt: WalletCreateAttempt,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError> {
        Ok(self
            .lock()?
            .wallet_create_attempts
            .insert(attempt.key.clone(), attempt))
    }

    fn insert_wallet_create_attempt_if_absent(
        &self,
        attempt: WalletCreateAttempt,
        max_pending_per_owner: u32,
    ) -> Result<InsertWalletCreateAttemptResult, PolicyStoreError> {
        use std::collections::hash_map::Entry;

        let mut state = self.lock()?;
        if let Some(existing) = state.wallet_create_attempts.get(&attempt.key) {
            return Ok(InsertWalletCreateAttemptResult::Existing(Box::new(
                existing.clone(),
            )));
        }
        let pending = state
            .wallet_create_attempts
            .values()
            .filter(|existing| {
                existing.key.network_id == attempt.key.network_id
                    && existing.key.genesis_hash == attempt.key.genesis_hash
                    && existing.key.owner_public_key == attempt.key.owner_public_key
                    && matches!(
                        existing.status,
                        WalletCreateAttemptStatus::Prepared
                            | WalletCreateAttemptStatus::PrefundPending
                            | WalletCreateAttemptStatus::PrefundConfirmed
                            | WalletCreateAttemptStatus::CreatePending
                    )
            })
            .count();
        if pending >= max_pending_per_owner as usize {
            return Ok(InsertWalletCreateAttemptResult::PendingLimitReached);
        }
        match state.wallet_create_attempts.entry(attempt.key.clone()) {
            Entry::Occupied(existing) => Ok(InsertWalletCreateAttemptResult::Existing(Box::new(
                existing.get().clone(),
            ))),
            Entry::Vacant(vacant) => {
                vacant.insert(attempt);
                Ok(InsertWalletCreateAttemptResult::Inserted)
            }
        }
    }

    fn claim_wallet_create_transition(
        &self,
        key: &WalletCreateAttemptKey,
        expected: &[WalletCreateAttemptStatus],
        next: WalletCreateAttemptStatus,
        now: SystemTime,
    ) -> Result<WalletCreateAttempt, PolicyStoreError> {
        let mut state = self.lock()?;
        let attempt = state
            .wallet_create_attempts
            .get_mut(key)
            .ok_or(PolicyStoreError::AttemptNotFound)?;
        if !expected.contains(&attempt.status) {
            return Err(PolicyStoreError::InvalidWalletCreateTransition {
                expected: expected.to_vec(),
                current: attempt.status,
            });
        }
        attempt.status = next;
        attempt.updated_at = now;
        Ok(attempt.clone())
    }

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
        max_pending_per_recipient: u32,
        quota_checks: &[QuotaCheck],
        now: SystemTime,
    ) -> Result<AdmitAirdropAttemptResult, PolicyStoreError> {
        let mut state = self.lock()?;
        if let Some(existing) = state.airdrop_attempts.get(&attempt.key) {
            return Ok(AdmitAirdropAttemptResult::Existing(Box::new(
                existing.clone(),
            )));
        }
        let pending = state
            .airdrop_attempts
            .values()
            .filter(|existing| {
                existing.key.network_id == attempt.key.network_id
                    && existing.key.genesis_hash == attempt.key.genesis_hash
                    && existing.key.recipient == attempt.key.recipient
                    && existing.status == AirdropAttemptStatus::Pending
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
        for check in quota_checks {
            state
                .quota_events
                .entry(check.key.clone())
                .or_default()
                .push(QuotaEvent {
                    occurred_at: now,
                    expires_at: now.checked_add(check.window).unwrap_or(now),
                });
        }
        state.airdrop_attempts.insert(attempt.key.clone(), attempt);
        Ok(AdmitAirdropAttemptResult::Inserted)
    }

    fn claim_airdrop_transition(
        &self,
        key: &AirdropAttemptKey,
        expected: &[AirdropAttemptStatus],
        next: AirdropAttemptStatus,
        now: SystemTime,
    ) -> Result<AirdropAttempt, PolicyStoreError> {
        let mut state = self.lock()?;
        let attempt = state
            .airdrop_attempts
            .get_mut(key)
            .ok_or(PolicyStoreError::AttemptNotFound)?;
        if !expected.contains(&attempt.status) {
            return Err(PolicyStoreError::InvalidAirdropTransition {
                expected: expected.to_vec(),
                current: attempt.status,
            });
        }
        attempt.status = next;
        attempt.updated_at = now;
        Ok(attempt.clone())
    }

    fn check_and_record_quota(
        &self,
        key: QuotaKey,
        max: u32,
        window: Duration,
        now: SystemTime,
    ) -> Result<QuotaDecision, PolicyStoreError> {
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
            expires_at: now.checked_add(window).unwrap_or(now),
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
        for check in checks {
            state
                .quota_events
                .entry(check.key.clone())
                .or_default()
                .push(QuotaEvent {
                    occurred_at: now,
                    expires_at: now.checked_add(check.window).unwrap_or(now),
                });
        }
        Ok(QuotaDecision {
            allowed: true,
            retry_after: None,
        })
    }

    fn expire(&self, now: SystemTime) -> Result<(), PolicyStoreError> {
        let mut state = self.lock()?;
        state
            .wallet_create_attempts
            .retain(|_, attempt| attempt.expires_at.is_none_or(|expires_at| expires_at > now));
        state.airdrop_attempts.retain(|_, attempt| {
            matches!(attempt.status, AirdropAttemptStatus::Pending)
                || attempt.expires_at.is_none_or(|expires_at| expires_at > now)
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
    fn wallet_create_transition_claim_updates_status_atomically() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let key = wallet_create_key();
        store
            .put_wallet_create_attempt(WalletCreateAttempt {
                key: key.clone(),
                deploy_nonce: "0x01".into(),
                wallet_address: "0x02".into(),
                wallet_contract_hash: "0x03".into(),
                prefund_raw_amount: "100000000".into(),
                status: WalletCreateAttemptStatus::Prepared,
                prefund_tx_hash: None,
                prefund_height: None,
                create_tx_hash: None,
                create_height: None,
                error_code: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
            })
            .expect("bug: insert should succeed");

        let claimed = store
            .claim_wallet_create_transition(
                &key,
                &[WalletCreateAttemptStatus::Prepared],
                WalletCreateAttemptStatus::PrefundPending,
                now + Duration::from_secs(1),
            )
            .expect("bug: transition should be valid");

        assert_eq!(claimed.status, WalletCreateAttemptStatus::PrefundPending);
    }

    #[test]
    fn wallet_create_transition_rejects_unexpected_status() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let key = wallet_create_key();
        store
            .put_wallet_create_attempt(WalletCreateAttempt {
                key: key.clone(),
                deploy_nonce: "0x01".into(),
                wallet_address: "0x02".into(),
                wallet_contract_hash: "0x03".into(),
                prefund_raw_amount: "100000000".into(),
                status: WalletCreateAttemptStatus::Created,
                prefund_tx_hash: None,
                prefund_height: None,
                create_tx_hash: None,
                create_height: None,
                error_code: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
            })
            .expect("bug: insert should succeed");

        let err = store
            .claim_wallet_create_transition(
                &key,
                &[WalletCreateAttemptStatus::Prepared],
                WalletCreateAttemptStatus::PrefundPending,
                now,
            )
            .expect_err("bug: transition should fail");

        assert!(matches!(
            err,
            PolicyStoreError::InvalidWalletCreateTransition { .. }
        ));
    }

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
                tx_hash: None,
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
                AirdropAttemptStatus::Succeeded,
                now + Duration::from_secs(1),
            )
            .expect("bug: transition should be valid");

        assert_eq!(claimed.status, AirdropAttemptStatus::Succeeded);
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
                        .admit_airdrop_attempt(airdrop_attempt("0xnonce"), 1, &checks, now)
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
                .admit_airdrop_attempt(airdrop_attempt("0xsecond"), 1, &checks, now)
                .expect("bug: second admission should succeed"),
            AdmitAirdropAttemptResult::Inserted
        ));
        assert!(matches!(
            store
                .admit_airdrop_attempt(airdrop_attempt("0xthird"), 1, &checks, now)
                .expect("bug: quota rejection should succeed"),
            AdmitAirdropAttemptResult::QuotaExceeded(_)
        ));
    }

    #[test]
    fn expire_removes_only_attempts_with_elapsed_retention_and_quota_events() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        let wallet_key = wallet_create_key();
        let airdrop_key = airdrop_key();
        let stale_airdrop_key = AirdropAttemptKey {
            client_nonce: "0xstale".into(),
            ..airdrop_key.clone()
        };
        store
            .put_wallet_create_attempt(WalletCreateAttempt {
                key: wallet_key.clone(),
                deploy_nonce: "0x01".into(),
                wallet_address: "0x02".into(),
                wallet_contract_hash: "0x03".into(),
                prefund_raw_amount: "100000000".into(),
                status: WalletCreateAttemptStatus::Prepared,
                prefund_tx_hash: None,
                prefund_height: None,
                create_tx_hash: None,
                create_height: None,
                error_code: None,
                created_at: now,
                updated_at: now,
                expires_at: Some(now + Duration::from_secs(5)),
            })
            .expect("bug: insert should succeed");
        store
            .put_airdrop_attempt(AirdropAttempt {
                key: airdrop_key.clone(),
                client_identity: ClientIdentity::Unknown,
                raw_amount: "100000000".into(),
                status: AirdropAttemptStatus::FailedTerminal,
                tx_hash: None,
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
                tx_hash: None,
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
                .get_wallet_create_attempt(&wallet_key)
                .expect("bug: lookup should succeed")
                .is_none()
        );
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

    fn wallet_create_key() -> WalletCreateAttemptKey {
        WalletCreateAttemptKey {
            network_id: "devnet-preview".into(),
            genesis_hash: "0xgenesis".into(),
            owner_public_key: "0xowner".into(),
            client_nonce: "0xnonce".into(),
        }
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
            tx_hash: None,
            height: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
        }
    }
}
