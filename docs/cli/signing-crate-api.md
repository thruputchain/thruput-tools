# Groundhog Signing Crate API

Concrete implementation design for the first extraction of
`groundhog-signing`.

This document assumes:

- `groundhog-signing` lives in the `groundhog-rpc` workspace first
- the initial Ed25519 backend is `ed25519-dalek`
- `tx-builder` becomes a thin wrapper over the new crate
- the CLI adapts its existing key-file format into the shared crate

Date: 2026-04-08

## Goals

The first version of `groundhog-signing` must be sufficient for:

- canonical pubkey-account address derivation
- building a signed deploy transaction
- building a signed invocation transaction
- encoding signed transactions to raw XDR bytes for RPC submission
- gateway load generation and test tooling
- CLI integration without re-implementing transaction semantics

It does not need to solve:

- hardware wallets
- KMS backends
- mnemonic UX
- RPC submission
- filesystem key management
- async signer implementations

## Crate Placement

Initial target:

```text
groundhog-rpc/
  crates/
    signing/
    tx-builder/
```

Suggested package name:

- `groundhog-signing`

Suggested crate path:

- `groundhog_signing`

## Dependencies

Required:

- `groundhog-xdr`
- `xdrs`
- `xdr-common`
- `sha2`
- `thiserror`
- `ed25519-dalek = "2"`
- `zeroize`

Not allowed in the crate:

- `tokio`
- `clap`
- `jsonrpsee`
- `serde_json`
- filesystem access
- env-var driven behavior

## Module Layout

```text
crates/signing/src/
  lib.rs
  error.rs
  indices.rs
  signer.rs
  local.rs
  account.rs
  hash.rs
  build.rs
  encode.rs
```

### `error.rs`

Owns the crate error type.

```rust
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error("invalid secret key bytes")]
    InvalidSecretKey,
    #[error("invalid keypair bytes")]
    InvalidKeypair,
    #[error("xdr encode failed: {0}")]
    XdrEncode(String),
}
```

Keep it narrow in v1. Expand only when call-site handling requires
distinguishing cases.

### `indices.rs`

Avoid bare positional `u32` arguments for signature metadata.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthClaimIndex(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKeyId(pub u32);
```

Provide sensible defaults:

```rust
impl Default for AuthClaimIndex {
    fn default() -> Self { Self(0) }
}

impl Default for PublicKeyId {
    fn default() -> Self { Self(0) }
}
```

### `signer.rs`

Define the synchronous signer abstraction used in v1.

```rust
pub trait Signer {
    fn public_key(&self) -> groundhog_xdr::types::PublicKey;
    fn sign(&self, message: &[u8]) -> Result<groundhog_xdr::types::Signature, SigningError>;
}
```

Forward-compatibility note:

- document that an async companion trait may be added later
- do not add async requirements to the sync local path yet

Optional placeholder:

```rust
pub trait AsyncSigner {
    fn public_key_async(
        &self,
    ) -> impl core::future::Future<
        Output = Result<groundhog_xdr::types::PublicKey, SigningError>
    >;

    fn sign_async(
        &self,
        message: &[u8],
    ) -> impl core::future::Future<
        Output = Result<groundhog_xdr::types::Signature, SigningError>
    >;
}
```

This does not need implementors in v1.

### `local.rs`

Concrete local signer backed by `ed25519-dalek::SigningKey`.

```rust
pub struct LocalSigner {
    signing_key: ed25519_dalek::SigningKey,
}

impl LocalSigner {
    pub fn from_secret_key(secret: [u8; 32]) -> Self;
    pub fn from_keypair_bytes(bytes: [u8; 64]) -> Result<Self, SigningError>;
    pub fn to_keypair_bytes(&self) -> [u8; 64];
    pub fn verifying_key_bytes(&self) -> [u8; 32];
}
```

Behavior:

- zeroize temporary buffers on parse paths
- convert output into Groundhog XDR `PublicKey` and `Signature` types
- do not expose filesystem APIs here

### `account.rs`

Canonical pubkey-account helpers.

```rust
pub const DEFAULT_PUBKEY_ACCOUNT_NONCE: u64 = 0;
pub const PUBKEY_ACCOUNT_TAG_STR: &str = "PUBKEY_ACCOUNT_TAG";

pub struct PubkeyAccount {
    pub public_key: groundhog_xdr::types::PublicKey,
    pub nonce: u64,
}

impl PubkeyAccount {
    pub fn new(public_key: groundhog_xdr::types::PublicKey, nonce: u64) -> Self;
    pub fn preimage(&self) -> groundhog_xdr::auth_claim::PubkeyAccountPreimage;
    pub fn address(&self) -> Result<groundhog_xdr::types::ContractAddress, SigningError>;
    pub fn auth_claim(&self) -> groundhog_xdr::auth_claim::AuthClaim;
}
```

Implementation detail:

- derive address from XDR encoding of `PubkeyAccountPreimage`
- hash with SHA-256
- XOR with `sha256("PUBKEY_ACCOUNT_TAG")`

This must replace the CLI’s hand-rolled preimage encoding.

### `hash.rs`

Canonical transaction hashing helpers.

```rust
pub fn transaction_hash(
    tx: &groundhog_xdr::transaction::Transaction,
) -> Result<groundhog_xdr::types::SHA256Hash, SigningError>;
```

Hash the encoded inner `Transaction`, not the full
`SignedTransaction`.

### `build.rs`

Own the protocol-level transaction assembly helpers.

```rust
pub struct SignTransactionParams {
    pub auth_claim_idx: AuthClaimIndex,
    pub public_key_id: PublicKeyId,
}

impl Default for SignTransactionParams {
    fn default() -> Self {
        Self {
            auth_claim_idx: AuthClaimIndex(0),
            public_key_id: PublicKeyId(0),
        }
    }
}

pub fn sign_transaction(
    tx: groundhog_xdr::transaction::Transaction,
    signer: &impl Signer,
    params: SignTransactionParams,
) -> Result<groundhog_xdr::transaction::SignedTransaction, SigningError>;
```

Builder inputs should be explicit structs, not long positional argument
lists.

#### Deploy

```rust
pub struct BuildDeployTransaction {
    pub invoked_address: groundhog_xdr::types::ContractAddress,
    pub entrypoint: groundhog_xdr::contract::ContractEntrypointID,
    pub calldata: Vec<u8>,
    pub gas_limit: u64,
    pub block_expiry_height: i64,
    pub nonce: u64,
    pub contracts_to_deploy: xdr_common::VecM<groundhog_xdr::transaction::ContractToDeploy>,
    pub auth_claims: xdr_common::VecM<groundhog_xdr::auth_claim::AuthClaim>,
}

pub fn build_deploy_transaction(
    args: BuildDeployTransaction,
) -> groundhog_xdr::transaction::Transaction;
```

#### Call

```rust
pub struct BuildCallTransaction {
    pub invoked_address: groundhog_xdr::types::ContractAddress,
    pub entrypoint: groundhog_xdr::contract::ContractEntrypointID,
    pub calldata: Vec<u8>,
    pub gas_limit: u64,
    pub block_expiry_height: i64,
    pub nonce: u64,
    pub auth_claims: xdr_common::VecM<groundhog_xdr::auth_claim::AuthClaim>,
}

pub fn build_call_transaction(
    args: BuildCallTransaction,
) -> groundhog_xdr::transaction::Transaction;
```

Note:

- the signing crate should not resolve contract specs or method names
- it should accept already-constructed entrypoint IDs and calldata

### `encode.rs`

```rust
pub fn encode_transaction_xdr(
    tx: &groundhog_xdr::transaction::Transaction,
) -> Result<Vec<u8>, SigningError>;

pub fn encode_signed_transaction_xdr(
    tx: &groundhog_xdr::transaction::SignedTransaction,
) -> Result<Vec<u8>, SigningError>;
```

These are the final helpers the CLI and loadgen need before submitting
bytes via the RPC client.

## `lib.rs` Exports

Suggested public exports:

```rust
pub mod account;
pub mod build;
pub mod encode;
pub mod error;
pub mod hash;
pub mod indices;
pub mod local;
pub mod signer;

pub use account::{
    PubkeyAccount,
    DEFAULT_PUBKEY_ACCOUNT_NONCE,
    PUBKEY_ACCOUNT_TAG_STR,
};
pub use build::{
    build_call_transaction,
    build_deploy_transaction,
    sign_transaction,
    BuildCallTransaction,
    BuildDeployTransaction,
    SignTransactionParams,
};
pub use encode::{encode_signed_transaction_xdr, encode_transaction_xdr};
pub use error::SigningError;
pub use hash::transaction_hash;
pub use indices::{AuthClaimIndex, PublicKeyId};
pub use local::LocalSigner;
pub use signer::{AsyncSigner, Signer};
```

## Extraction From `tx-builder`

Move the following logic out of the current binary:

- `pubkey_account_tag()`
- `derive_address()`
- raw `Transaction` construction
- `SignedTransaction` construction
- XDR encoding

The `tx-builder` binary should keep only:

- CLI arg parsing
- seed/account-name derivation for its test-only convenience path
- printing stdout/stderr formatting

### New `tx-builder` Flow

1. derive a local signer from its test seed inputs
2. build `PubkeyAccount`
3. build transaction with `groundhog-signing`
4. sign with `groundhog-signing`
5. encode and print XDR
6. print tx hash and account address

## CLI Integration Design

The CLI keeps ownership of key files.

Current file shape:

```json
{
  "version": 1,
  "scheme": "ed25519",
  "keypair_hex": "...",
  "public_key_hex": "..."
}
```

The bridge should look like:

```rust
fn load_signer(name: &str) -> Result<(groundhog_signing::LocalSigner, KeyInfo), CliError>;
```

Implementation plan:

1. parse stored `keypair_hex`
2. convert into `[u8; 64]`
3. call `LocalSigner::from_keypair_bytes`
4. derive public key and address through `groundhog-signing`

CLI should stop owning:

- address derivation
- XDR preimage encoding details
- direct signing key usage outside storage/parsing

## Test Plan

### Required tests in `groundhog-signing`

1. address derivation from known public key + nonce
2. pubkey auth claim construction
3. transaction hash over a known `Transaction`
4. signed transaction assembly for one auth claim
5. signed XDR encoding

### Canonical vector requirement

Do not rely only on self-consistent roundtrip tests.

At least one of these must happen in the first extraction batch:

1. import runtime-derived vectors from an existing Groundhog vector
   source, or
2. add vector fixtures generated from the canonical runtime/XDR path and
   committed in a shared location

Good candidate homes:

- `sdk_rs/groundhog-test-vectors`
- a new shared fixture file in the gateway repo copied from runtime
  outputs

Minimum vectors:

- public key -> address
- transaction -> tx hash
- transaction + signing key -> signed transaction XDR

### Cross-check target

The address derivation test should assert that the new crate matches:

- the current gateway `tx-builder` semantics
- the old SDK wallet branch semantics
- the executor/runtime expectation

## Implementation Batch

### Batch A

In `groundhog-rpc`:

1. add `crates/signing`
2. implement:
   - `error.rs`
   - `indices.rs`
   - `signer.rs`
   - `local.rs`
   - `account.rs`
   - `hash.rs`
   - `encode.rs`
   - `build.rs`
3. port `tx-builder` to the new crate
4. add focused tests

Exit criteria:

- `tx-builder` produces the same output as before for existing inputs
- crate tests cover canonical helpers

### Batch B

In `groundhog-cli`:

1. replace local address derivation with `groundhog-signing`
2. replace `load_key()` with `load_signer()`
3. keep command behavior identical for `keys generate/ls/use/rm`

Exit criteria:

- CLI key output is unchanged
- CLI no longer directly owns signing-domain behavior

## Deferred Work

- async signer implementors
- Ledger / KMS / secure-store backends
- mnemonic-based local signer helpers
- higher-level wallet UX
- repo consolidation mechanics
