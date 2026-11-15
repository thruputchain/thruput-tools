#![cfg_attr(not(feature = "dev"), no_std)]

thruput_sdk::declare_mock_contract!();

mod implementation;

pub use implementation::AwardedEvent;
pub use implementation::MAX_AWARD;
pub use implementation::Points;
pub use implementation::PointsClient;
pub use implementation::PointsError;
