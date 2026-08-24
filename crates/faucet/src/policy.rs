use std::collections::{HashMap, HashSet};
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
pub enum InsertAirdropAttemptResult {
    Inserted,
    Existing(Box<AirdropAttempt>),
    PendingLimitReached,
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
    /// Look up an attempt, evicting it first when it is no longer live
    /// at `now` (expired, or terminal for airdrops). Callers therefore
    /// never observe stale attempts even between background sweeps.
    fn get_wallet_create_attempt(
        &self,
        key: &WalletCreateAttemptKey,
        now: SystemTime,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError>;

    fn put_wallet_create_attempt(
        &self,
        attempt: WalletCreateAttempt,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError>;

    /// Insert a new attempt unless the key already holds a live one,
    /// enforcing the per-owner pending cap against attempts live at
    /// `now`.
    fn insert_wallet_create_attempt_if_absent(
        &self,
        attempt: WalletCreateAttempt,
        max_pending_per_owner: u32,
        now: SystemTime,
    ) -> Result<InsertWalletCreateAttemptResult, PolicyStoreError>;

    fn claim_wallet_create_transition(
        &self,
        key: &WalletCreateAttemptKey,
        expected: &[WalletCreateAttemptStatus],
        next: WalletCreateAttemptStatus,
        now: SystemTime,
    ) -> Result<WalletCreateAttempt, PolicyStoreError>;

    /// Look up an attempt, evicting it first when it is no longer live
    /// at `now` (expired or terminally failed).
    fn get_airdrop_attempt(
        &self,
        key: &AirdropAttemptKey,
        now: SystemTime,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError>;

    fn put_airdrop_attempt(
        &self,
        attempt: AirdropAttempt,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError>;

    /// Insert a new attempt unless the key already holds a live one,
    /// enforcing the per-recipient pending cap against attempts live at
    /// `now`.
    fn insert_airdrop_attempt_if_absent(
        &self,
        attempt: AirdropAttempt,
        max_pending_per_recipient: u32,
        now: SystemTime,
    ) -> Result<InsertAirdropAttemptResult, PolicyStoreError>;

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

    /// Sweep out every attempt and quota event no longer live at `now`.
    ///
    /// Reclaims memory for keys that are never touched again; per-key
    /// correctness does not depend on it because reads evict lazily.
    /// Run it from a periodic background task, not per request.
    fn expire(&self, now: SystemTime) -> Result<(), PolicyStoreError>;
}

#[derive(Debug, Default)]
pub struct InMemoryFaucetPolicyStore {
    state: Mutex<PolicyState>,
}

#[derive(Debug, Default)]
struct PolicyState {
    wallet_create_attempts: HashMap<WalletCreateAttemptKey, WalletCreateAttempt>,
    /// Attempt keys grouped by owner, so per-owner pending checks scan
    /// one owner's attempts instead of the whole store.
    wallet_create_owner_index: HashMap<WalletCreateOwnerKey, HashSet<WalletCreateAttemptKey>>,
    airdrop_attempts: HashMap<AirdropAttemptKey, AirdropAttempt>,
    /// Attempt keys grouped by recipient, so per-recipient pending
    /// checks scan one recipient's attempts instead of the whole store.
    airdrop_recipient_index: HashMap<AirdropRecipientKey, HashSet<AirdropAttemptKey>>,
    quota_events: HashMap<QuotaKey, Vec<QuotaEvent>>,
}

/// Grouping key for one owner's wallet-create attempts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WalletCreateOwnerKey {
    network_id: String,
    genesis_hash: String,
    owner_public_key: String,
}

impl From<&WalletCreateAttemptKey> for WalletCreateOwnerKey {
    fn from(key: &WalletCreateAttemptKey) -> Self {
        Self {
            network_id: key.network_id.clone(),
            genesis_hash: key.genesis_hash.clone(),
            owner_public_key: key.owner_public_key.clone(),
        }
    }
}

/// Grouping key for one recipient's airdrop attempts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AirdropRecipientKey {
    network_id: String,
    genesis_hash: String,
    recipient: String,
}

impl From<&AirdropAttemptKey> for AirdropRecipientKey {
    fn from(key: &AirdropAttemptKey) -> Self {
        Self {
            network_id: key.network_id.clone(),
            genesis_hash: key.genesis_hash.clone(),
            recipient: key.recipient.clone(),
        }
    }
}

/// Whether a wallet-create attempt is still live at `now`.
fn wallet_create_attempt_is_live(attempt: &WalletCreateAttempt, now: SystemTime) -> bool {
    attempt.expires_at.is_none_or(|expires_at| expires_at > now)
}

/// Whether an airdrop attempt is still live at `now`. Terminal
/// failures are not live: evicting them lets the same client nonce
/// retry from scratch.
fn airdrop_attempt_is_live(attempt: &AirdropAttempt, now: SystemTime) -> bool {
    !matches!(attempt.status, AirdropAttemptStatus::FailedTerminal)
        && attempt.expires_at.is_none_or(|expires_at| expires_at > now)
}

fn wallet_create_attempt_is_pending(attempt: &WalletCreateAttempt) -> bool {
    matches!(
        attempt.status,
        WalletCreateAttemptStatus::Prepared
            | WalletCreateAttemptStatus::PrefundPending
            | WalletCreateAttemptStatus::PrefundConfirmed
            | WalletCreateAttemptStatus::CreatePending
    )
}

impl PolicyState {
    fn remove_wallet_create(&mut self, key: &WalletCreateAttemptKey) {
        self.wallet_create_attempts.remove(key);
        let owner = WalletCreateOwnerKey::from(key);
        if let Some(keys) = self.wallet_create_owner_index.get_mut(&owner) {
            keys.remove(key);
            if keys.is_empty() {
                self.wallet_create_owner_index.remove(&owner);
            }
        }
    }

    fn insert_wallet_create(
        &mut self,
        attempt: WalletCreateAttempt,
    ) -> Option<WalletCreateAttempt> {
        self.wallet_create_owner_index
            .entry(WalletCreateOwnerKey::from(&attempt.key))
            .or_default()
            .insert(attempt.key.clone());
        self.wallet_create_attempts
            .insert(attempt.key.clone(), attempt)
    }

    fn remove_airdrop(&mut self, key: &AirdropAttemptKey) {
        self.airdrop_attempts.remove(key);
        let recipient = AirdropRecipientKey::from(key);
        if let Some(keys) = self.airdrop_recipient_index.get_mut(&recipient) {
            keys.remove(key);
            if keys.is_empty() {
                self.airdrop_recipient_index.remove(&recipient);
            }
        }
    }

    fn insert_airdrop(&mut self, attempt: AirdropAttempt) -> Option<AirdropAttempt> {
        self.airdrop_recipient_index
            .entry(AirdropRecipientKey::from(&attempt.key))
            .or_default()
            .insert(attempt.key.clone());
        self.airdrop_attempts.insert(attempt.key.clone(), attempt)
    }

    /// Count this owner's live pending attempts, evicting any of the
    /// owner's attempts that are no longer live at `now`.
    fn live_pending_wallet_creates(
        &mut self,
        owner: &WalletCreateOwnerKey,
        now: SystemTime,
    ) -> usize {
        let keys: Vec<WalletCreateAttemptKey> = self
            .wallet_create_owner_index
            .get(owner)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default();
        let mut pending = 0;
        for key in keys {
            match self.wallet_create_attempts.get(&key) {
                Some(attempt) if wallet_create_attempt_is_live(attempt, now) => {
                    if wallet_create_attempt_is_pending(attempt) {
                        pending += 1;
                    }
                }
                _ => self.remove_wallet_create(&key),
            }
        }
        pending
    }

    /// Count this recipient's live pending attempts, evicting any of
    /// the recipient's attempts that are no longer live at `now`.
    fn live_pending_airdrops(&mut self, recipient: &AirdropRecipientKey, now: SystemTime) -> usize {
        let keys: Vec<AirdropAttemptKey> = self
            .airdrop_recipient_index
            .get(recipient)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default();
        let mut pending = 0;
        for key in keys {
            match self.airdrop_attempts.get(&key) {
                Some(attempt) if airdrop_attempt_is_live(attempt, now) => {
                    if attempt.status == AirdropAttemptStatus::Pending {
                        pending += 1;
                    }
                }
                _ => self.remove_airdrop(&key),
            }
        }
        pending
    }
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
        now: SystemTime,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError> {
        let mut state = self.lock()?;
        match state.wallet_create_attempts.get(key) {
            Some(attempt) if wallet_create_attempt_is_live(attempt, now) => {
                Ok(Some(attempt.clone()))
            }
            Some(_) => {
                state.remove_wallet_create(key);
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn put_wallet_create_attempt(
        &self,
        attempt: WalletCreateAttempt,
    ) -> Result<Option<WalletCreateAttempt>, PolicyStoreError> {
        Ok(self.lock()?.insert_wallet_create(attempt))
    }

    fn insert_wallet_create_attempt_if_absent(
        &self,
        attempt: WalletCreateAttempt,
        max_pending_per_owner: u32,
        now: SystemTime,
    ) -> Result<InsertWalletCreateAttemptResult, PolicyStoreError> {
        let mut state = self.lock()?;
        match state.wallet_create_attempts.get(&attempt.key) {
            Some(existing) if wallet_create_attempt_is_live(existing, now) => {
                return Ok(InsertWalletCreateAttemptResult::Existing(Box::new(
                    existing.clone(),
                )));
            }
            Some(_) => state.remove_wallet_create(&attempt.key),
            None => {}
        }
        let owner = WalletCreateOwnerKey::from(&attempt.key);
        if state.live_pending_wallet_creates(&owner, now) >= max_pending_per_owner as usize {
            return Ok(InsertWalletCreateAttemptResult::PendingLimitReached);
        }
        state.insert_wallet_create(attempt);
        Ok(InsertWalletCreateAttemptResult::Inserted)
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
        now: SystemTime,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError> {
        let mut state = self.lock()?;
        match state.airdrop_attempts.get(key) {
            Some(attempt) if airdrop_attempt_is_live(attempt, now) => Ok(Some(attempt.clone())),
            Some(_) => {
                state.remove_airdrop(key);
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn put_airdrop_attempt(
        &self,
        attempt: AirdropAttempt,
    ) -> Result<Option<AirdropAttempt>, PolicyStoreError> {
        Ok(self.lock()?.insert_airdrop(attempt))
    }

    fn insert_airdrop_attempt_if_absent(
        &self,
        attempt: AirdropAttempt,
        max_pending_per_recipient: u32,
        now: SystemTime,
    ) -> Result<InsertAirdropAttemptResult, PolicyStoreError> {
        let mut state = self.lock()?;
        match state.airdrop_attempts.get(&attempt.key) {
            Some(existing) if airdrop_attempt_is_live(existing, now) => {
                return Ok(InsertAirdropAttemptResult::Existing(Box::new(
                    existing.clone(),
                )));
            }
            Some(_) => state.remove_airdrop(&attempt.key),
            None => {}
        }
        let recipient = AirdropRecipientKey::from(&attempt.key);
        if state.live_pending_airdrops(&recipient, now) >= max_pending_per_recipient as usize {
            return Ok(InsertAirdropAttemptResult::PendingLimitReached);
        }
        state.insert_airdrop(attempt);
        Ok(InsertAirdropAttemptResult::Inserted)
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
            .retain(|_, attempt| wallet_create_attempt_is_live(attempt, now));
        state
            .airdrop_attempts
            .retain(|_, attempt| airdrop_attempt_is_live(attempt, now));
        // Rebuild the grouping indexes from the surviving attempts so
        // index entries for swept keys do not accumulate.
        state.wallet_create_owner_index.clear();
        state.airdrop_recipient_index.clear();
        let wallet_keys: Vec<WalletCreateAttemptKey> =
            state.wallet_create_attempts.keys().cloned().collect();
        for key in wallet_keys {
            let owner = WalletCreateOwnerKey::from(&key);
            state
                .wallet_create_owner_index
                .entry(owner)
                .or_default()
                .insert(key);
        }
        let airdrop_keys: Vec<AirdropAttemptKey> = state.airdrop_attempts.keys().cloned().collect();
        for key in airdrop_keys {
            let recipient = AirdropRecipientKey::from(&key);
            state
                .airdrop_recipient_index
                .entry(recipient)
                .or_default()
                .insert(key);
        }
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
    fn expire_removes_expired_attempts_terminal_airdrops_and_quota_events() {
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
                status: AirdropAttemptStatus::Pending,
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
                .get_wallet_create_attempt(&wallet_key, now)
                .expect("bug: lookup should succeed")
                .is_none()
        );
        assert!(
            store
                .get_airdrop_attempt(&airdrop_key, now)
                .expect("bug: lookup should succeed")
                .is_none()
        );
        assert!(
            store
                .get_airdrop_attempt(&stale_airdrop_key, now)
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

    fn wallet_attempt(
        key: WalletCreateAttemptKey,
        status: WalletCreateAttemptStatus,
        expires_at: Option<SystemTime>,
    ) -> WalletCreateAttempt {
        let now = SystemTime::UNIX_EPOCH;
        WalletCreateAttempt {
            key,
            deploy_nonce: "0x01".into(),
            wallet_address: "0x02".into(),
            wallet_contract_hash: "0x03".into(),
            prefund_raw_amount: "100000000".into(),
            status,
            prefund_tx_hash: None,
            prefund_height: None,
            create_tx_hash: None,
            create_height: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            expires_at,
        }
    }

    fn airdrop_attempt(
        key: AirdropAttemptKey,
        status: AirdropAttemptStatus,
        expires_at: Option<SystemTime>,
    ) -> AirdropAttempt {
        let now = SystemTime::UNIX_EPOCH;
        AirdropAttempt {
            key,
            client_identity: ClientIdentity::Unknown,
            raw_amount: "100000000".into(),
            status,
            tx_hash: None,
            height: None,
            error_code: None,
            created_at: now,
            updated_at: now,
            expires_at,
        }
    }

    fn wallet_create_key_with_nonce(nonce: &str) -> WalletCreateAttemptKey {
        WalletCreateAttemptKey {
            client_nonce: nonce.into(),
            ..wallet_create_key()
        }
    }

    #[test]
    fn insert_enforces_the_pending_cap_per_owner() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;

        let first = store
            .insert_wallet_create_attempt_if_absent(
                wallet_attempt(
                    wallet_create_key_with_nonce("0x01"),
                    WalletCreateAttemptStatus::Prepared,
                    None,
                ),
                1,
                now,
            )
            .expect("bug: first insert should succeed");
        assert_eq!(first, InsertWalletCreateAttemptResult::Inserted);

        // A second nonce for the same owner hits the pending cap.
        let second = store
            .insert_wallet_create_attempt_if_absent(
                wallet_attempt(
                    wallet_create_key_with_nonce("0x02"),
                    WalletCreateAttemptStatus::Prepared,
                    None,
                ),
                1,
                now,
            )
            .expect("bug: second insert should succeed");
        assert_eq!(second, InsertWalletCreateAttemptResult::PendingLimitReached);

        // A different owner has its own budget.
        let other_owner = WalletCreateAttemptKey {
            owner_public_key: "0xother".into(),
            ..wallet_create_key()
        };
        let other = store
            .insert_wallet_create_attempt_if_absent(
                wallet_attempt(other_owner, WalletCreateAttemptStatus::Prepared, None),
                1,
                now,
            )
            .expect("bug: other-owner insert should succeed");
        assert_eq!(other, InsertWalletCreateAttemptResult::Inserted);

        // Completing the first attempt frees the owner's pending slot.
        store
            .claim_wallet_create_transition(
                &wallet_create_key_with_nonce("0x01"),
                &[WalletCreateAttemptStatus::Prepared],
                WalletCreateAttemptStatus::Created,
                now,
            )
            .expect("bug: transition should be valid");
        let after_completion = store
            .insert_wallet_create_attempt_if_absent(
                wallet_attempt(
                    wallet_create_key_with_nonce("0x02"),
                    WalletCreateAttemptStatus::Prepared,
                    None,
                ),
                1,
                now,
            )
            .expect("bug: post-completion insert should succeed");
        assert_eq!(after_completion, InsertWalletCreateAttemptResult::Inserted);
    }

    #[test]
    fn pending_cap_ignores_attempts_that_expired_since_the_last_sweep() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        store
            .put_airdrop_attempt(airdrop_attempt(
                airdrop_key(),
                AirdropAttemptStatus::Pending,
                Some(now + Duration::from_secs(5)),
            ))
            .expect("bug: seed insert should succeed");

        let result = store
            .insert_airdrop_attempt_if_absent(
                airdrop_attempt(
                    AirdropAttemptKey {
                        client_nonce: "0xfresh".into(),
                        ..airdrop_key()
                    },
                    AirdropAttemptStatus::Pending,
                    None,
                ),
                1,
                now + Duration::from_secs(6),
            )
            .expect("bug: insert should succeed");

        // The stale attempt no longer counts against the cap, and the
        // check evicted it without waiting for the background sweep.
        assert_eq!(result, InsertAirdropAttemptResult::Inserted);
        assert!(
            store
                .get_airdrop_attempt(&airdrop_key(), now)
                .expect("bug: lookup should succeed")
                .is_none()
        );
    }

    #[test]
    fn reads_evict_expired_and_terminal_attempts_without_a_sweep() {
        let store = InMemoryFaucetPolicyStore::new();
        let now = SystemTime::UNIX_EPOCH;
        store
            .put_wallet_create_attempt(wallet_attempt(
                wallet_create_key(),
                WalletCreateAttemptStatus::Prepared,
                Some(now + Duration::from_secs(5)),
            ))
            .expect("bug: seed insert should succeed");
        store
            .put_airdrop_attempt(airdrop_attempt(
                airdrop_key(),
                AirdropAttemptStatus::FailedTerminal,
                None,
            ))
            .expect("bug: seed insert should succeed");

        assert!(
            store
                .get_wallet_create_attempt(&wallet_create_key(), now + Duration::from_secs(6))
                .expect("bug: lookup should succeed")
                .is_none(),
            "expired wallet-create attempt must be evicted on read"
        );
        assert!(
            store
                .get_airdrop_attempt(&airdrop_key(), now)
                .expect("bug: lookup should succeed")
                .is_none(),
            "terminal airdrop attempt must be evicted on read"
        );

        // Evicted keys accept a fresh attempt immediately.
        let retried = store
            .insert_airdrop_attempt_if_absent(
                airdrop_attempt(airdrop_key(), AirdropAttemptStatus::Pending, None),
                1,
                now,
            )
            .expect("bug: retry insert should succeed");
        assert_eq!(retried, InsertAirdropAttemptResult::Inserted);
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
}
