//! Canonical off-chain signing and transaction construction helpers for
//! Thruput.

pub mod account;
pub mod build;
pub mod default_wallet;
pub mod dev_account;
pub mod dev_wallet;
pub mod encode;
pub mod error;
pub mod hash;
pub mod indices;
pub mod local;
pub mod mldsa;
pub mod native_token;
pub mod signer;
pub mod strkey;

pub use account::{DEFAULT_PUBKEY_ACCOUNT_NONCE, PubkeyAccount, public_key_hash};
pub use build::{
    BuildCallTransaction, BuildDeployTransaction, BuildDevWalletDoActionTransaction,
    DEFAULT_TX_GAS_LIMIT, SignTransactionParams, WALLET_DEPLOY_ENTRYPOINT, build_call_transaction,
    build_deploy_transaction, build_dev_wallet_do_action_transaction, derive_deploy_address,
    dev_wallet_do_action_calldata, sign_and_encode_transaction_hex, sign_transaction,
    source_address_auth_claim,
};
pub use dev_account::{
    DEFAULT_DEV_ACCOUNT_COUNT, DEFAULT_DEV_ACCOUNT_SEED, derive_default_dev_account_signer,
    derive_dev_account_signer,
};
pub use dev_wallet::{
    NATIVE_TOKEN_DECIMALS, NATIVE_TOKEN_SCALE, NATIVE_TOKEN_TRANSFER_ENTRYPOINT,
    native_token_transfer_calldata, wallet_contract_hash,
};
pub use encode::{encode_signed_transaction_xdr, encode_transaction_xdr};
pub use error::SigningError;
pub use hash::transaction_hash;
pub use indices::PublicKeyId;
pub use local::{LocalSigner, SchemeSigner};
pub use mldsa::{
    MLDSA65_PUBLIC_KEY_LEN, MLDSA65_SEED_LEN, MLDSA65_SIGNATURE_LEN, MlDsa65Signer, verify_mldsa65,
};
pub use native_token::{ParseNativeTokenAmountError, parse_native_token_amount};
pub use signer::Signer;
