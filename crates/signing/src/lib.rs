//! Canonical off-chain signing and transaction construction helpers for
//! Groundhog.

pub mod account;
pub mod build;
pub mod default_wallet;
pub mod dev_account;
pub mod dev_wallet;
pub mod encode;
pub mod error;
pub mod hash;
pub mod hog;
pub mod indices;
pub mod local;
pub mod mldsa;
pub mod signer;
pub mod strkey;

pub use account::{DEFAULT_PUBKEY_ACCOUNT_NONCE, PubkeyAccount, public_key_hash};
pub use build::{
    BuildCallTransaction, BuildDeployTransaction, BuildDevWalletDoActionTransaction,
    DEFAULT_TX_GAS_LIMIT, SignTransactionParams, WALLET_DEPLOY_ENTRYPOINT, WalletCreateProofV2,
    build_call_transaction, build_deploy_transaction, build_dev_wallet_do_action_transaction,
    derive_deploy_address, dev_wallet_do_action_calldata, sign_and_encode_transaction_hex,
    sign_transaction, source_address_auth_claim, wallet_create_proof_message,
    wallet_create_proof_message_v2,
};
pub use dev_account::{
    DEFAULT_DEV_ACCOUNT_COUNT, DEFAULT_DEV_ACCOUNT_SEED, DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS,
    derive_default_dev_account_signer, derive_dev_account_signer,
};
pub use dev_wallet::{
    BuildSignedDevWalletHogTransfer, NATIVE_HOG_ADDRESS, NATIVE_HOG_DECIMALS, NATIVE_HOG_SCALE,
    NATIVE_HOG_TRANSFER_ENTRYPOINT, SignedDevWalletHogTransfer,
    build_signed_dev_wallet_hog_transfer, create_wallet_calldata, dev_wallet_contract,
    native_hog_transfer_calldata, wallet_contract_hash,
};
pub use encode::{encode_signed_transaction_xdr, encode_transaction_xdr};
pub use error::SigningError;
pub use hash::transaction_hash;
pub use hog::{ParseHogAmountError, parse_hog_amount};
pub use indices::{AuthClaimIndex, PublicKeyId};
pub use local::{LocalSigner, SchemeSigner};
pub use mldsa::{
    MLDSA65_PUBLIC_KEY_LEN, MLDSA65_SEED_LEN, MLDSA65_SIGNATURE_LEN, MlDsa65Signer, verify_mldsa65,
};
pub use signer::Signer;
