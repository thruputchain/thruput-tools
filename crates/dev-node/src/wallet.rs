//! Embedded dev-wallet WASM binary.
//!
//! The dev-wallet contract is cross-compiled by build.rs from
//! `dev-wallet/` (bundled inside this crate) and embedded here as a constant.

/// Dev-wallet WASM bytecode, cross-compiled from the bundled `dev-wallet/` source.
pub const DEV_WALLET_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dev_wallet.wasm"));
