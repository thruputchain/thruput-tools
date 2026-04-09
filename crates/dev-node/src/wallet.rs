//! Embedded dev-wallet WASM binary.
//!
//! The dev-wallet contract is cross-compiled by build.rs from
//! `crates/dev-wallet/` and embedded here as a constant.

/// Dev-wallet WASM bytecode, compiled from `crates/dev-wallet/`.
pub const DEV_WALLET_WASM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/dev_wallet.wasm"));
