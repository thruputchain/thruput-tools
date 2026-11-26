#![cfg_attr(not(feature = "dev"), no_std)]

use thruput_sdk::storage::{Map, NNInt};
use thruput_sdk::*;

thruput_sdk::declare_mock_contract!();

/// Each caller owns an independent count, so `bump()` then `get()` is a
/// per-caller read-after-write that concurrent callers never interfere with.
#[thruput_sdk::storage]
pub struct CallerCounter {
    counts: Map<ContractAddress, NNInt>,
}

#[contract]
impl CallerCounter {
    /// Increment the caller's counter by one.
    pub fn bump(&mut self) {
        self.counts.at_mut(&get_caller()).strict_inc(1);
    }

    /// Return the caller's current counter value, or zero if never bumped.
    pub fn get(&self) -> u64 {
        self.counts
            .at(&get_caller())
            .try_get()
            .none_if_missing()
            .unwrap_or(0) as u64
    }
}
