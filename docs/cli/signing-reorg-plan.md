# Groundhog Signing And Repo Reorg Plan

Design plan for the next phase of the Groundhog developer tooling
stack. Focuses on:

- repo boundaries
- shared signing and transaction construction
- migration from current CLI and gateway state

Date: 2026-04-08

## Summary

Recommendation:

1. Merge `groundhog-cli` and `groundhog-rpc` into a single off-chain
   developer-tools workspace.
2. Keep `sdk_rs` separate for now.
3. Add a shared `groundhog-signing` crate to the merged workspace.
4. Move the current gateway `tx-builder` logic into that crate and make
   `tx-builder` a thin binary wrapper or remove it once callers migrate.
5. Make the CLI depend on `groundhog-signing` for canonical address
   derivation, auth-claim construction, transaction hashing, and
   transaction signing.

This follows the pattern used by other Rust-heavy chains and toolchains:
crate separation for concerns, fewer repo boundaries than we currently
have.

## Why

Current state:

- The CLI roadmap already says signing and tx-building should come from
  a shared library extracted from
  `groundhog-rpc/crates/tx-builder`.
- The CLI currently duplicates signing-domain logic locally:
  - its own Ed25519 dependency
  - its own address derivation
  - future tx-signing would otherwise also land there
- The gateway already has the canonical XDR-oriented path, but only in a
  binary crate.

This creates the wrong incentives:

- transaction-domain code gets duplicated in the CLI
- the gateway's current implementation is hard to reuse
- cross-repo coordination slows down the next critical path item

## Repo Boundary Recommendation

### Keep Separate

Keep `sdk_rs` as its own repo for now.

Reasons:

- different center of gravity
- primarily contract authoring, macros, native execution, and test
  environment
- less coupled to JSON-RPC transport and CLI UX

### Combine

Combine `groundhog-cli` and `groundhog-rpc` into a single workspace.

Reasons:

- both are off-chain developer tooling
- both change together for deploy/call work
- both need the same signing and transaction-construction logic
- the same people are currently coordinating both sides
- no meaningful external-user stability constraint exists yet

### Resulting Workspace

Suggested target layout:

```text
groundhog-devtools/
  crates/
    api-types/
    client/
    gateway/
    signing/
    cli/
    tx-builder/   # temporary thin wrapper, optional long-term
    loadgen/      # later
```

The repo does not need to be renamed immediately. The lowest-friction
version is to keep the current `groundhog-rpc` repo and move the CLI
crate into it.

## Why Not A New Signing Repo Yet

A new standalone repo is premature.

A separate crate is the important architectural boundary. A separate repo
adds:

- another release pipeline
- another versioning surface
- another place for roadmap drift
- another cross-repo PR dependency

Most Rust chain ecosystems prefer crate boundaries first and repo
boundaries only when ownership, release cadence, or access control
actually demand it.

## Why Not Put This In The SDK

The shared signing crate should consume canonical protocol and XDR
types, but it should not live inside the main contract SDK surface.

Reasons:

- signing is off-chain tooling logic
- RPC gateway loadgen and CLI need it
- it should stay free of contract-authoring concerns
- it should not drag CLI and gateway ergonomics into the core SDK

Daniel's earlier `wallet` branch in `sdk_rs` is still useful as source
material, but not necessarily the right final home.

## Signing Crate Scope

Suggested crate name: `groundhog-signing`

Purpose: canonical off-chain signing and transaction construction for
Groundhog.

### In Scope

- signer abstraction for Groundhog transaction signing
- local Ed25519 signer implementation
- canonical pubkey-account address derivation from
  `PubkeyAccountPreimage`
- auth-claim helpers for pubkey-signature accounts
- transaction hash helper
- transaction builders for:
  - deploy
  - contract call
- signed transaction assembly
- XDR encoding helpers for raw submission

### Out Of Scope

- JSON-RPC transport
- polling transaction status
- CLI prompts and user interaction
- XDG config
- key naming and default-key selection
- OS keychain integration
- mnemonic UX and hardware wallet UX in the first batch

## Crate Design

### Dependency Policy

`groundhog-signing` should stay small and deterministic.

Allowed:

- `groundhog-xdr`
- `xdrs`
- `xdr-common`
- `sha2`
- one Ed25519 library
- optional `zeroize`
- `thiserror`

Avoid:

- `tokio`
- `jsonrpsee`
- `clap`
- filesystem access
- env-var based configuration

### Feature Flags

Keep the default feature set narrow.

Suggested first-pass features:

- `std` default
- `xdr-encode` default
- `zeroize` default if required by the chosen Ed25519 backend

Guidance:

- callers that only need key handling or transaction construction should
  not be forced to pull in extra serialization helpers unless those
  helpers are actually used
- transport, filesystem, and CLI behavior should remain out of scope

### Ed25519 Choice

Do not lock the design to `ed25519-compact`.

Current local state:

- the CLI uses `ed25519-dalek`
- the gateway `tx-builder` uses `ed25519-compact`
- Daniel's earlier `wallet` crate used `ed25519-compact`
- the current Rust coordinator tree in `chuckd-rs` uses
  `ed25519-dalek`

Current recommendation:

- prefer the library already standardized in the broader Groundhog Rust
  stack
- current lean: `ed25519-dalek`
- do not preserve `ed25519-compact` just because the current
  `tx-builder` happens to use it

Decision criteria:

- alignment with the rest of Groundhog's Rust stack
- mature and well-understood signing API
- compatibility with the wider Rust `signature` trait ecosystem where
  useful
- compatibility with zeroization and secure key handling
- minimal conversion friction to Groundhog XDR types
- low-risk maintenance story

Implication for migration:

- the gateway `tx-builder` code is still the right protocol source to
  extract from
- but the extraction may swap out its Ed25519 backend as part of the
  move into `groundhog-signing`

### Module Layout

Suggested first-pass module layout:

```text
signing/
  src/
    lib.rs
    error.rs
    signer.rs
    local.rs
    account.rs
    auth.rs
    hash.rs
    build.rs
    encode.rs
```

### Public API Shape

Keep the initial API explicit and protocol-shaped. Avoid a giant
stateful "wallet" object in v1.

Suggested public items:

```rust
pub trait Signer {
    fn public_key(&self) -> PublicKey;
    fn sign(&self, message: &[u8]) -> Result<Signature, SigningError>;
}

/// Optional future companion for hardware or remote signers.
pub trait AsyncSigner {
    async fn public_key(&self) -> Result<PublicKey, SigningError>;
    async fn sign(&self, message: &[u8]) -> Result<Signature, SigningError>;
}

pub struct LocalSigner { ... }

impl LocalSigner {
    pub fn from_secret_key(secret: [u8; 32]) -> Result<Self, SigningError>;
    pub fn from_keypair_bytes(bytes: [u8; 64]) -> Result<Self, SigningError>;
    pub fn to_keypair_bytes(&self) -> [u8; 64];
}

pub struct PubkeyAccount {
    pub public_key: PublicKey,
    pub nonce: u64,
}

impl PubkeyAccount {
    pub fn address(&self) -> ContractAddress;
    pub fn auth_claim(&self) -> AuthClaim;
}

pub fn transaction_hash(tx: &Transaction) -> Result<SHA256Hash, SigningError>;

pub struct AuthClaimIndex(pub u32);
pub struct PublicKeyId(pub u32);

pub fn sign_transaction(
    tx: Transaction,
    signer: &impl Signer,
    auth_claim_idx: AuthClaimIndex,
    public_key_id: PublicKeyId,
) -> Result<SignedTransaction, SigningError>;

pub fn build_deploy_transaction(... ) -> Result<Transaction, SigningError>;
pub fn build_call_transaction(... ) -> Result<Transaction, SigningError>;
pub fn encode_signed_transaction_xdr(
    signed_tx: &SignedTransaction,
) -> Result<Vec<u8>, SigningError>;
```

The v1 API should be narrow. It only needs to support the CLI deploy and
call path plus gateway loadgen.

The crate does not need async implementors in v1, but it should be
documented and structured so an async signer companion can be added
without contorting the sync local-signer path later.

## Responsibilities By Crate

### `groundhog-signing`

- protocol-aware off-chain transaction construction
- signing and hashing
- canonical address derivation

### `groundhog-client`

- submit raw signed transaction bytes
- fetch status and receipts

### `groundhog-cli`

- load key material from disk
- select default key
- map `groundhog.toml` profile and alias config into tx-builder inputs
- print results and hints

### `groundhog-gateway`

- accept raw signed transaction bytes
- decode and forward
- no private key handling

## Review Against Current Code

### CLI

Current issues:

- `src/keys.rs` uses `ed25519-dalek`
- `src/keys.rs` derives addresses locally
- signing-oriented key loading is present but only as future-use helper

Implication:

- CLI should keep key file parsing and persistence
- address derivation and transaction signing should move out
- `load_key()` should become an adapter that returns a
  `groundhog-signing::LocalSigner` or raw key bytes consumable by it

### Gateway

Current issues:

- `crates/tx-builder` is a binary, not a reusable library
- it already contains the right protocol-shaped logic
- it is described as "jank" and a temporary extraction

Implication:

- it is the correct bootstrap point for extraction
- the binary should stop owning real logic

### SDK branch source material

Daniel's `origin/dbg/wal` branch is good reference material for:

- keypair wrapper
- account abstraction
- pubkey-account address derivation
- signed transaction assembly

But its final shape should be modernized:

- no monolithic mutable wallet required for v1
- no SDK-specific stateful nonce manager in the first public API
- explicit builders are better than hidden state for CLI and loadgen use

## Migration Plan

### Batch 1: Extract Shared Signing Logic

In the gateway workspace:

1. Create `crates/signing`.
2. Move canonical helpers out of `crates/tx-builder`.
3. Add tests for:
   - address derivation
   - tx hash
   - signed transaction assembly
   - XDR roundtrip
   - runtime-derived canonical vectors where available
4. Make `tx-builder` depend on `signing`.

Exit criteria:

- `tx-builder` becomes a thin wrapper
- canonical logic has library tests
- tests are anchored to canonical vectors rather than only
  self-consistent roundtrips

### Batch 2: Adapt CLI To Shared Signing

In the CLI:

1. replace local address derivation with `groundhog-signing`
2. replace direct CLI Ed25519 usage with the shared crate path
3. bridge stored key files into `LocalSigner`

Exit criteria:

- no signing-domain duplication remains in CLI
- CLI key listing still works and emits the same addresses

### Batch 3: Implement Deploy

1. resolve active profile and RPC URL
2. load local signer from stored key
3. build deploy transaction with `groundhog-signing`
4. submit raw XDR with `groundhog-client`
5. poll status to committed or terminal failure

Exit criteria:

- usable `groundhog deploy`

### Batch 4: Implement Call

1. resolve profile and contract alias
2. build invocation transaction from spec/IDL plus CLI args
3. sign with shared crate
4. submit and print result

Exit criteria:

- usable `groundhog call`

### Batch 5: Repo Consolidation

Once Batch 1 is real and Batch 2 is underway:

1. move the CLI crate into the gateway workspace
2. keep crate names stable where possible
3. update CI and install docs
4. archive or stub the old CLI repo

This ordering keeps the important boundary change first and prevents the
repo move from obscuring the signing extraction work.

## Risks

### Risk: Repo move adds churn before behavior changes land

Mitigation:

- extract the signing crate first
- move repos after the shared boundary is proven

### Risk: Signing crate becomes a junk drawer

Mitigation:

- keep it pure and protocol-focused
- no RPC, no config, no CLI prompts

### Risk: SDK and devtools drift on canonical transaction semantics

Mitigation:

- depend on canonical XDR types
- add shared test vectors generated from runtime semantics where
  possible
- prefer vectors shared with or generated from protocol/runtime code,
  not only self-consistent crate-local fixtures

### Risk: Early API overbuild

Mitigation:

- only support local Ed25519 signer in v1
- add hardware/KMS backends later behind optional features if needed

## Open Questions

1. Ed25519 backend choice:
   - Groundhog-stack standard library
   - `ed25519-dalek`
   - avoid keeping `ed25519-compact` by inertia alone

2. Final crate name:
   - `groundhog-signing`
   - `groundhog-wallet`
   - `groundhog-tx-builder`

3. Key-file format ownership:
   - keep owned by CLI
   - or move a reusable parser into signing

Recommendation:

- keep key-file storage owned by CLI in v1

4. Whether the repo consolidation should happen before or after deploy
   lands.

Recommendation:

- after shared signing extraction begins, before too much more
  cross-repo work accumulates

## Recommended Next Implementation Step

The next concrete implementation batch should be:

1. extract `groundhog-signing` from gateway `tx-builder`
2. add golden tests for address derivation and signed XDR
3. update the CLI roadmap wording to reflect the chosen direction

Do not start by adding direct tx-signing to the CLI.
