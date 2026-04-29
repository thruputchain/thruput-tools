# RPC Server Extraction Plan

**Date:** 2026-04-28
**Status:** Draft implementation plan for review.

**Companion to:**
- [`../2026-04-25-hogd-dev-node-hybrid.md`](../2026-04-25-hogd-dev-node-hybrid.md)

## Goal

Extract a reusable Rust JSON-RPC server layer so the production gateway and
dev-node compositions can share one implementation of `GroundhogApiServer`.

This is Stage 1 from the hogd-owned hybrid dev-node proposal. It should stand
alone as a behavior-preserving refactor: production gateway behavior remains
unchanged, and the future `hogd-dev` backend should consume the shared server
instead of adding another hand-written `GroundhogApiServer` implementation.

Migrating the current `native_exec` dev-node is a separate decision. It is
worth doing if that backend remains a supported user-facing path during the
transition. It is not a prerequisite if the next dev-node implementation is a
new supervisor that starts `hogd-dev` and the Rust JSON-RPC facade.

## Current State

The workspace already has shared RPC-facing crates:

- `crates/api-types` defines JSON-facing request and response types.
- `crates/rpc-api` defines the jsonrpsee traits and generated clients.
- `crates/client` provides the typed JSON-RPC client.

The missing piece is the shared server-side presentation layer.

Today:

- `crates/gateway/src/api.rs` implements `GroundhogApiServer`.
- `crates/gateway/src/handlers/*` contain production handler logic backed by
  hogd scheduler and indexer x23 clients.
- `crates/dev-node/src/rpc.rs` separately implements `GroundhogApiServer`
  over `WorkerHandle`.
- `crates/dev-node/src/rpc.rs` also implements `GroundhogDevApiServer`.

The duplicated core API implementation is small today, but it already has
duplicated input handling and error mapping. It will become more expensive to
keep in sync if `hogd-dev` is introduced as another backend or replacement.

## Target Shape

```text
crates/rpc-server
  - implements GroundhogApiServer once
  - owns public request validation
  - owns JSON-RPC error mapping
  - owns raw transaction hex/XDR validation and tx hash computation
  - depends on backend traits, not concrete hogd or dev-node wiring

crates/gateway
  - remains the production binary
  - owns config, startup, middleware, metrics, and x23 client wiring
  - owns adaptive batching and tip coalescing
  - implements rpc-server backend traits with production adapters

crates/dev-node
  - remains the current native_exec-backed dev-node unless retired
  - may implement rpc-server backend traits if it remains supported
  - keeps or later delegates groundhog_dev_* methods through DevControl

crates/<hogd-dev supervisor> (name TBD)
  - starts and supervises hogd-dev
  - owns deterministic account presentation and child-process lifecycle
  - implements rpc-server backend traits with local hogd-dev x23 clients
  - adds or delegates groundhog_dev_* methods through DevControl
```

The important boundary: `rpc-server` is the public JSON-RPC handler layer, not
the production gateway runtime. Gateway-specific operational concerns such as
`BatchSubmitter`, `TipCoalescer`, HTTP middleware, x23 connection pools, and
Prometheus integration should stay in `crates/gateway`.

## Proposed Crate Layout

Add `crates/rpc-server` with package name `groundhog-rpc-server`.

Initial modules:

- `backend.rs` - backend traits and validated request types.
- `config.rs` - server-layer validation config, initially event range bounds.
- `error.rs` - shared RPC server errors mapped to `api-types` error codes.
- `server.rs` - generic `GroundhogApiServer` implementation.
- `tx.rs` - raw transaction parsing, XDR validation, and tx hash computation.
- `lib.rs` - public exports.

Workspace dependency:

```toml
# Use the same exact-version path dependency convention as the other
# published workspace crates.
groundhog-rpc-server = { path = "crates/rpc-server", version = "=0.2.5", registry = "groundhog" }
```

Expected direct dependencies:

- `groundhog-rpc-api`
- `groundhog-rpc-api-types`
- `groundhog-xdr`
- `xdrs`
- `jsonrpsee`
- `async-trait`
- `hex`
- `sha2`
- `thiserror`
- `tracing`

## Backend Traits

Start with separate traits for the public core API. This matches the backend
shape in the hybrid design and avoids one large trait that every adapter must
fully care about.

```rust
pub struct ValidatedTransaction {
    pub bytes: Vec<u8>,
    pub hash: TxHash,
}

#[async_trait::async_trait]
pub trait SubmitPlane: Send + Sync + 'static {
    async fn submit_raw_transaction(
        &self,
        tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError>;
}

#[async_trait::async_trait]
pub trait ChainPlane: Send + Sync + 'static {
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError>;

    async fn get_block_by_number(
        &self,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError>;

    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError>;
}

#[async_trait::async_trait]
pub trait ReadPlane: Send + Sync + 'static {
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError>;

    async fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError>;

    async fn contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> Result<Vec<IdentifiedEvent>, RpcServerError>;
}
```

The first pass can either require one backend type to implement all three
traits, or define a marker trait:

```rust
pub trait GroundhogBackend: SubmitPlane + ChainPlane + ReadPlane {}
impl<T> GroundhogBackend for T where T: SubmitPlane + ChainPlane + ReadPlane {}
```

All public RPC type names in these traits should come from
`groundhog-rpc-api-types`. Do not introduce duplicate block height, address, or
hash newtypes in `rpc-server`.

### Submit Hash Contract

`rpc-server` computes the canonical transaction hash before backend dispatch.
`ValidatedTransaction::hash` is the public response hash and the source of
truth for the shared server.

`SubmitPlane::submit_raw_transaction` returns `Result<(), RpcServerError>`,
not a hash. Backend adapters should either:

- pass the server-computed hash through to lower layers and avoid recomputing
  it for response purposes; or
- during a transition, compare any lower-layer recomputed hash with
  `ValidatedTransaction::hash` and treat a mismatch as an internal backend
  bug.

The preferred long-term shape is to pass `ValidatedTransaction` through the
backend path and avoid duplicate hash computation. This already fits the
gateway batcher, which consumes both transaction bytes and hash. If the current
`native_exec` dev-node is migrated, its worker command should eventually accept
the precomputed hash rather than returning a second hash to the RPC layer.

### Read Semantics Contract

`ReadPlane::transaction_status` is a semantic backend operation, not a raw
single-source lookup. `rpc-server` should not orchestrate pending-versus-
committed fallback itself.

For the production gateway adapter, `transaction_status` should preserve the
current composite behavior: check scheduler/mempool best-effort for `Pending`,
then check the indexer as the authoritative committed source. Other backends,
including `native_exec` and `hogd-dev`, can implement the same semantic answer
through their own local state or read APIs.

`pending_transaction_count` remains a normal backend method returning
`Result<u64, RpcServerError>`. A backend that cannot answer it should return
`RpcServerError::NotImplemented`. The dev-node can continue to return `0` if
that is correct for its no-mempool implementation. This keeps capability
absence explicit instead of encoding not-implemented as `Option::None`.

For dev-only methods, introduce `DevControl` either in the same extraction or
as a follow-up once the core API migration is stable:

```rust
#[async_trait::async_trait]
pub trait DevControl: Send + Sync + 'static {
    async fn reset(&self) -> Result<(), RpcServerError>;
    async fn accounts(&self) -> Result<Vec<DevAccount>, RpcServerError>;
    async fn transaction_detail(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionDetail>, RpcServerError>;
    async fn storage_at(
        &self,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<Option<StorageItemInfo>, RpcServerError>;
}
```

## Shared Server Responsibilities

`rpc-server` should own:

- `GroundhogApiServer` implementation.
- Hex decoding for `sendRawTransaction`.
- XDR decoding of `SignedTransaction`.
- Canonical `TxHash` computation:
  `sha256(xdrs::txs::encode(signed_tx.tx))`.
- Oversized transaction rejection before XDR work using the existing
  `u32::try_from(tx_bytes.len())` limit required by the gateway batch format.
  The current dev-node does not enforce this limit; sharing validation closes
  that gap.
- Public JSON-RPC error mapping to `api-types` error codes.
- Event range validation:
  - `to_height >= from_height`
  - range does not exceed configured maximum
- Generic not-implemented errors for backend methods that are intentionally
  deferred.

`rpc-server` should not own:

- hogd scheduler or indexer generated client types.
- x23 connection pooling or reconnect policy.
- gateway batching implementation.
- gateway tip coalescing implementation.
- gateway HTTP middleware.
- native_exec worker internals.
- CLI process supervision.

## Error Model

Create a shared `RpcServerError` enum equivalent to the public error surface
currently split across gateway and dev-node errors:

- `TxInvalid(String)`
- `TxRejected(String)`
- `TxNotFound`
- `BlockNotFound`
- `InvalidParam(String)`
- `RangeTooLarge(String)`
- `BackendUnavailable(String)`
- `BackendTimeout`
- `Overloaded(String)`
- `NotImplemented(String)`

Map these to the existing constants in `groundhog_rpc_api_types::error::codes`.

Adapter-specific errors should be converted into `RpcServerError` at the edge.
For example, gateway x23 failures become `BackendUnavailable`, submitter queue
pressure becomes `Overloaded`, and current dev-node worker shutdown becomes
`BackendUnavailable`.

Do not fully replace gateway or dev-node internal error enums in the first
extraction. Keep internal errors where they carry useful local context, then
convert at the trait boundary:

```text
gateway/dev-node internal error -> RpcServerError -> ErrorObjectOwned
```

The migration should preserve existing JSON-RPC error codes for all current
client-visible failures.

## Gateway Migration

Add a gateway adapter type, likely:

```rust
pub struct GatewayBackend {
    ctx: Arc<GatewayContext>,
}
```

It should implement the shared traits:

- `SubmitPlane::submit_raw_transaction`
  - call `ctx.batch_submitter.submit(tx.bytes, tx.hash).await`
  - preserve current batching and overload behavior
- `ChainPlane::chain_tip`
  - call `ctx.tip_coalescer.chain_tip().await`
- `ChainPlane::get_block_by_number`
  - return `RpcServerError::NotImplemented("getBlockByNumber".into())`
- `ChainPlane::pending_transaction_count`
  - return `RpcServerError::NotImplemented("pendingTransactionCount".into())`
- `ReadPlane::transaction_receipt`
  - keep current indexer lookup and receipt construction
- `ReadPlane::transaction_status`
  - keep current scheduler best-effort pending check, then indexer lookup
- `ReadPlane::contract_events`
  - keep current indexer call and event parsing

The status method is intentionally adapter-owned orchestration. The shared
server should call `ReadPlane::transaction_status` once; it should not know
that the gateway currently combines scheduler and indexer reads.

Gateway code that should remain gateway-owned:

- `context.rs`
- `config.rs`
- `middleware.rs`
- `submitter.rs`
- `tip_coalescer.rs`
- hogd indexer and scheduler generated client imports
- `parse_event.rs`

Gateway code that can shrink or disappear:

- `api.rs` should be removed or reduced to a type alias/re-export.
- validation currently in `handlers/submit.rs` moves to `rpc-server`.
- event range validation currently in `handlers/events.rs` moves to
  `rpc-server`; hogd query and event parsing stay gateway-owned.

`main.rs` should switch from:

```rust
let api_impl = GroundhogApiImpl::new(ctx);
let handle = server.start(api_impl.into_rpc());
```

to:

```rust
let backend = GatewayBackend::new(ctx);
let api = GroundhogRpcServer::new(backend, rpc_config);
let handle = server.start(api.into_rpc());
```

## Current Dev-Node Migration

This migration is conditional.

The current `crates/dev-node` is the `native_exec` prototype backend. The
durable plan may replace it with a new supervisor composition that starts one
`hogd-dev` child process plus the Rust JSON-RPC server. That supervisor might
live in a new crate or binary name so the existing prototype can remain
available until it is deliberately retired.

Migrate the current `native_exec` dev-node if at least one of these is true:

- it remains the implementation behind `groundhog node` for another release;
- it remains a supported fallback while `hogd-dev` is experimental;
- it is useful as the second backend proving that `rpc-server` is genuinely
  reusable before the hogd-dev supervisor exists.

Skip or defer this migration if the next implementation work is the new
`hogd-dev` supervisor and the current `native_exec` dev-node is treated as
legacy prototype code. In that case, the rule should be: no new dev-node
composition may add another hand-written `GroundhogApiServer`.

If the current implementation is left in place as legacy prototype code, mark
that boundary in source, not only in this plan. At minimum, add a module-level
doc comment near `crates/dev-node/src/rpc.rs::DevNodeRpc` stating that it is a
legacy prototype RPC implementation and that new dev-node compositions must
use `groundhog-rpc-server` instead of copying the pattern.

Add a dev-node adapter over `WorkerHandle`.

It should implement:

- `SubmitPlane::submit_raw_transaction`
  - preferred: pass `ValidatedTransaction` or its precomputed hash through the
    worker command and return `()`
  - acceptable bridge: call the current worker, compare the returned hash to
    `ValidatedTransaction::hash`, and fail on mismatch
- `ReadPlane::transaction_receipt`
  - delegate to `worker.get_transaction_receipt`
- `ReadPlane::transaction_status`
  - delegate to `worker.get_transaction_status`
- `ReadPlane::contract_events`
  - delegate to `worker.get_contract_events`
- `ChainPlane::chain_tip`
  - delegate to `worker.chain_tip`
- `ChainPlane::get_block_by_number`
  - return not implemented, preserving current behavior
- `ChainPlane::pending_transaction_count`
  - return `0`, preserving current behavior

The first migration can leave `GroundhogDevApiServer` implemented in
`crates/dev-node/src/rpc.rs` if that keeps the patch easier to review.

A follow-up can move `GroundhogDevApiServer` into `rpc-server` as a generic
wrapper over `DevControl`. That follow-up should also move storage key length
validation into `rpc-server`.

## Implementation Steps

These steps are implementation order, not necessarily review boundaries:

- Steps 1-4 are scoped to PR 1, unless the patch has to be split for review.
- Step 5 is PR 2 and should target either the current `native_exec` dev-node
  or the new `hogd-dev` supervisor.
- Steps 6-7 are follow-up work for PR 3 or later.
- PR 4 exists only if the old native prototype needs an explicit cleanup or
  legacy marker after the new supervisor path lands.

### Step 1 - Add `rpc-server` Crate

- Add `crates/rpc-server/Cargo.toml`.
- Register the crate in workspace `members`.
- Add workspace dependency entry.
- Add module skeleton and public exports.
- Add `RpcServerConfig` with `max_event_height_range`.

This step may compile with no consumers if split as a preparatory commit.
Prefer making the first reviewable PR include the gateway adapter too, so the
new crate has a real consumer in the same PR.

### Step 2 - Add Shared Transaction Validation

- Move hex decoding into `rpc-server::tx`.
- Move XDR validation into `rpc-server::tx`.
- Move canonical hash computation into `rpc-server::tx`.
- Return `ValidatedTransaction`.
- Add focused unit tests for invalid hex, invalid XDR, oversized tx, and hash
  calculation.

This step can be folded into the generic server step in the implementation
history if a standalone validation module with no caller feels too artificial.

### Step 3 - Add Generic Core API Server

- Implement `GroundhogRpcServer<B>`.
- Implement `GroundhogApiServer` for `GroundhogRpcServer<B>`.
- Add mock backend tests for:
  - valid submit calls backend with `ValidatedTransaction`
  - submit returns computed hash after backend acceptance
  - inverted event range returns invalid params
  - excessive event range returns range too large
  - backend not implemented maps to the existing not-implemented code

### Step 4 - Adapt Gateway

- Add `GatewayBackend`.
- Convert gateway backend errors into `RpcServerError`.
- Route `main.rs` through `GroundhogRpcServer`.
- Remove duplicate `GroundhogApiServer` implementation from gateway.
- Keep gateway public behavior unchanged.

Recommended verification:

```bash
cargo test -p groundhog-rpc-server
cargo test -p groundhog-rpc-gateway
```

### Step 5 - Adapt a Dev-Node Core API

Choose the target based on sequencing:

- If the current `native_exec` dev-node remains user-facing, add
  `DevNodeBackend` over `WorkerHandle` and route its core
  `GroundhogApiServer` through `GroundhogRpcServer`.
- If the new `hogd-dev` supervisor is the next dev-node path, wire that new
  composition to `GroundhogRpcServer` from the start and leave the current
  prototype alone until it is migrated or retired.

If adapting the current `native_exec` dev-node, keep dev-only RPC methods
working by merging:

- `GroundhogApiServer::into_rpc(shared_core_server)`
- `GroundhogDevApiServer::into_rpc(dev_rpc)`

Then remove the duplicate core API implementation from that dev-node.

Recommended verification:

```bash
cargo test -p groundhog-dev-node
cargo test -p groundhog-cli --features dev-node
```

### Step 6 - Optional Dev-Control Extraction

Once the core API migration is stable:

- Add `GroundhogDevRpcServer<D>`.
- Add `DevControl`.
- Move `groundhog_dev_*` validation and error mapping into `rpc-server`.
- Make dev-node implement `DevControl` over `WorkerHandle`.

This can be a separate PR if the core extraction is already large.

### Step 7 - Add Shared Conformance Coverage

Add tests that can exercise the same API behavior through different backends.

Useful assertions:

- invalid raw tx hex is rejected with the same public error code
- unknown tx status returns `Unknown` for backends that can answer reads
- unknown receipt returns `None` for backends that can answer reads
- `pendingTransactionCount` behavior remains backend-specific but documented:
  not implemented on gateway, `0` on current dev-node
- event range validation is identical before backend dispatch

The existing ignored e2e tests in `crates/e2e-tests` can remain black-box
tests against a running server. The new conformance tests should be cheap unit
or integration tests against mock backends where possible.

## Suggested PR Split

### PR 1 - Shared Core Server and Gateway Adapter

- Add `crates/rpc-server`.
- Add backend traits.
- Add shared errors.
- Add transaction validation helpers.
- Add generic core `GroundhogApiServer` implementation.
- Add mock backend tests.
- Add `GatewayBackend`.
- Switch gateway startup to `GroundhogRpcServer`.
- Remove gateway duplicate core API implementation.
- Preserve current production gateway behavior.

This combines the previously separate "shared crate" and "gateway adapter"
work so the new crate is not introduced unused. If the patch is too large for
review, split it into preparatory and gateway-consumer PRs, but call out the
temporary unused-crate window explicitly.

### PR 2 - Dev-Node Consumer

Pick one consumer based on project sequencing:

- **Current native_exec dev-node:** add `DevNodeBackend`, switch the core API
  to `GroundhogRpcServer`, keep `groundhog_dev_*` working, and preserve current
  behavior.
- **New hogd-dev supervisor:** create the new composition using
  `GroundhogRpcServer` from the start, without migrating the current prototype
  first.

The durable requirement is that the next supported dev-node composition uses
`rpc-server`; migrating the current prototype is valuable only if it continues
to be supported.

### PR 3 - Dev-Control and Conformance Tests

- Move dev-only RPC presentation into `rpc-server` if still desired.
- Add shared conformance tests.
- Document backend-specific behavior that intentionally differs.

### PR 4 - Native Prototype Cleanup, If Needed

- If PR 2 used the new `hogd-dev` supervisor, either migrate the old
  `native_exec` dev-node later or mark it legacy and remove it from supported
  user-facing paths.
- If PR 2 migrated the current `native_exec` dev-node, this PR may be
  unnecessary.

## Current Review Positions

1. Keep `SubmitPlane`, `ChainPlane`, and `ReadPlane` separate, plus the marker
   `GroundhogBackend` super-trait. This keeps mocks and future partial
   backends simpler than one large trait.

2. Defer `DevControl`. The load-bearing change is the core
   `GroundhogApiServer` extraction; dev-only methods currently have one
   consumer.

3. Use `RpcServerError` as the public-facing error at the shared RPC layer.
   Keep gateway/dev-node internal errors and convert at the trait boundary.

4. Do not rename gateway handler modules during the first migration. They can
   become helpers used by `GatewayBackend`; renaming would be mostly churn.

5. Put cheap conformance tests with mock backend fixtures in
   `crates/rpc-server/tests/`. Keep `crates/e2e-tests` as process-level black
   box tests. Do not add a test-support crate for this scope.

6. Treat current `native_exec` dev-node migration as conditional. It is worth
   doing if the prototype remains user-facing; otherwise the new `hogd-dev`
   supervisor should be the dev-node consumer of `rpc-server`.

## Acceptance Criteria

- Gateway uses `rpc-server` for `GroundhogApiServer`.
- The next supported dev-node composition uses `rpc-server` for
  `GroundhogApiServer`.
- Any remaining hand-written `GroundhogApiServer` implementation is either
  migrated or explicitly treated as legacy prototype code pending retirement.
- If the current `native_exec` implementation is left as legacy, that status
  is marked in source near its hand-written RPC implementation.
- Gateway starts and exposes the same public methods as before.
- If the current `native_exec` dev-node is migrated, it starts and exposes the
  same core and dev-only methods as before.
- Existing JSON-RPC error codes are preserved for current failures.
- `sendRawTransaction` validation and hash computation happen in one shared
  code path for all consumers of `rpc-server`.
- Event range validation happens in one shared code path.
- Gateway batching and tip coalescing remain gateway-owned.
- Dev-node `native_exec` behavior remains available until it is intentionally
  migrated, replaced, or retired.
- `cargo fmt --check` passes.
- `cargo test -p groundhog-rpc-server` passes.
- `cargo test -p groundhog-rpc-gateway` passes.
- If the current `native_exec` dev-node is touched,
  `cargo test -p groundhog-dev-node` passes.
- If the CLI dev-node path is touched,
  `cargo test -p groundhog-cli --features dev-node` passes.
