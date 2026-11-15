use thruput_sdk::storage::{Balance, Map};
use thruput_sdk::*;

/// Most points one award may carry.
pub const MAX_AWARD: u32 = 10;

#[contract_exception]
pub enum PointsError {
    /// A badge tried to award itself.
    SelfAward = 1,
    /// The amount was zero or above `MAX_AWARD`.
    BadAmount = 2,
    /// A badge id of zero.
    BadBadge = 3,
}

#[derive(Event, Clone, Debug)]
pub struct AwardedEvent {
    #[topic]
    pub from: u64,
    #[topic]
    pub to: u64,
    pub amount: u32,
}

/// Points that conference badges award each other.
///
/// A badge is named by its id, the ASCII characters of the id (`"7K2Q9M"`)
/// as a big-endian integer.
///
/// THIS CONTRACT AUTHENTICATES NOTHING. `open` sets the address's required
/// signature weight to zero, so anyone can submit a transaction to it with no
/// signature, and `from` is whatever the sender typed. The badge OS gives apps
/// no way to sign, so the badge app cannot do better. Demo use only.
#[derive(Debug)]
#[thruput_sdk::storage]
pub struct Points {
    points: Map<u64, Balance>,
    given: Map<u64, Balance>,
}

#[contract(throws = PointsError, emits(AwardedEvent))]
impl Points {
    /// Makes the contract callable with unsigned transactions. Run once as
    /// the deployment initializer; calling it again changes nothing.
    pub fn open(&mut self) {
        set_address_required_weight(0).unwrap();
    }

    /// Records `amount` points from badge `from` to badge `to`.
    pub fn award(&mut self, from: u64, to: u64, amount: u32) {
        if from == 0 || to == 0 {
            throw(PointsError::BadBadge);
        }
        if from == to {
            throw(PointsError::SelfAward);
        }
        if amount == 0 || amount > MAX_AWARD {
            throw(PointsError::BadAmount);
        }
        self.points.at_mut(&to).increase(amount as i128);
        self.given.at_mut(&from).increase(amount as i128);
        log_event(&AwardedEvent { from, to, amount });
    }

    /// Points awarded to `badge`.
    pub fn points(&self, badge: u64) -> u32 {
        saturate(self.points.at(&badge).get())
    }

    /// Points `badge` has awarded to others.
    pub fn given(&self, badge: u64) -> u32 {
        saturate(self.given.at(&badge).get())
    }
}

fn saturate(value: i128) -> u32 {
    if value > u32::MAX as i128 {
        u32::MAX
    } else {
        value as u32
    }
}
