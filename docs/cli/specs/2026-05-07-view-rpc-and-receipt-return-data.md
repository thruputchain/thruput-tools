# View RPC and Receipt Return Data

**Date:** 2026-05-07
**Status:** Draft, reviewed against current groundhog-rpc / hogd /
protocol-rs code. Needs hogd-side agreement on the protocol surface before
any Rust-side implementation begins.

**Companion to:**

- [`../roadmap.md`](../roadmap.md) — CLI roadmap; this spec defines the
  Phase-2 follow-up that unblocks `groundhog call <c> get` returning a
  decoded value.
- [`2026-05-03-dev-account-bootstrap.md`](2026-05-03-dev-account-bootstrap.md)
  — dev-account bootstrap; relevant because view calls are the first
  read path that does *not* depend on a funded dev account.

## Goal

Make read-only contract reads first-class in the Groundhog stack:

1. A user can call a getter on a deployed contract without holding a key,
   without funding an account, and without writing a transaction to chain
   history.
2. A user submitting a state-changing transaction can see the contract's
   return value on the resulting receipt, without re-executing or
   inspecting events.

The motivating CLI loop:

```
groundhog new my-counter && cd my-counter
groundhog build
groundhog node          # in another terminal
groundhog deploy        # writes alias counter -> address
groundhog call my-counter increment
groundhog view my-counter get   # -> 1
```

Today, step 5 is impossible — there is no read-only RPC — and step 4's
receipt does not surface the return value because the indexer's `TxInfo`
does not carry it.

## Why both halves

Comparable chains consistently expose a read-only contract path, but they
vary on whether committed transaction metadata carries return data:

| Chain | Read RPC | Write result surface |
|---|---|---|
| Ethereum | `eth_call` | Receipts do not include contract return data |
| Sui | `sui_devInspectTransactionBlock` | Dev inspect includes return values; committed effects do not model a root return value |
| Aptos | `POST /view` (Move `#[view]`) | Transaction responses do not expose ordinary function returns |
| NEAR | `query` / view functions | Execution outcomes can carry `SuccessValue` |
| CosmWasm | smart query | Execute responses can carry optional `data`, though events are the common app surface |
| Solana | `simulateTransaction` | Confirmed transaction metadata can include `meta.returnData` |

The DX argument for a dedicated read path is consistent across all of
them: reads should not require a key, should not commit, should not
pollute history, and should eventually be runnable against historical
heights. Receipt return-data alone does not satisfy any of those.

The DX argument for return-data on receipts is narrower but real: when a
state-changing call has a meaningful return value (`transfer` returning
the new balance, an admin op returning the new config version), forcing
callers to re-execute or to encode the result as an event is hostile.

The two halves share the same hogd-side dependency — exporting the root
`InvocationFrameOutput` out of the executor — so doing both costs only
marginally more than doing either one in isolation.

## Read path: view RPC and `groundhog view`

### RPC surface

```rust
#[method(name = "callView")]
async fn call_view(&self, params: CallViewRequest) -> RpcResult<CallViewResponse>;

pub struct CallViewRequest {
    pub contract:   Address,
    pub entrypoint: EntrypointId,
    pub calldata:   HexBytes,
    pub caller:     Option<Address>,        // simulated msg.sender
}

pub struct CallViewResponse {
    pub output:     RootInvocationOutput,    // see "Protocol surface" below
    pub height:     BlockHeight,             // height the read executed against
}
```

Method name follows the existing `groundhog_` prefix; `groundhog_callView`
is the wire name. Snake-case Rust to camelCase JSON via the existing
serde convention.

Semantics:

- No transaction is constructed, signed, or committed.
- No `AuthClaim` is validated. The contract's executor sees the target
  frame as if its parent frame address were `caller` (or as a root
  invocation with no msg.sender, when `caller` is `None`).
- Storage reads are served from the latest committed snapshot. Historical
  view calls are out of scope for v1 because the current public
  `DbGateway` explicitly exposes latest-committed reads only.
- The invocation runs with hogexec's read-only behavior. State-modifying
  syscalls are rejected; writes are not silently executed and discarded.
- Gas accounting runs as in production under a server-configured view gas
  cap, but no gas is charged.
- Return shape mirrors `InvocationFrameOutput` (`Return` / `Abort` /
  `Fault`) so the caller sees the same three outcomes a transaction
  would.

### Why `caller` instead of full auth simulation

Groundhog's auth model is source-address-based (`AuthClaim::
kSourceAddressAuthData`) and hogexec's current `get_msg_sender`
implementation returns the previous invocation frame's address; root
frames have no msg.sender. For read scenarios this means the only piece
of caller context that matters is the `msg.sender` a target contract
sees when invoked as a subinvocation. We expose that directly as
`caller` and do not model wallets, AuthClaims, signatures, or
capabilities.

This is a service-level execution mode, not a transaction shape. If
`caller` is present, hogd must either create an equivalent synthetic
parent-frame context for the target invocation or otherwise make
`get_msg_sender` return that address for the view frame. Passing a
`caller` to an ordinary root invocation is not sufficient.

Sui-style "execute the wallet's `do_action` end-to-end with a faked
auth claim" is explicitly out of scope for v1 (see "Out of scope"
below). It is strictly less useful than `caller=<wallet-address>` for
typical read scenarios, and supporting it would require a "skip auth
validation" mode in the executor.

### CLI grammar

```
groundhog view <contract> <method> [args...]
groundhog view <contract> <method> [args...] --from <addr-or-dev-N>
groundhog view <contract> <method> [args...] --json
```

- `<contract>` resolves through the same alias table `groundhog call`
  uses (`groundhog.toml` per-profile aliases).
- `--from` accepts a hex address or a `dev-N` shorthand, resolved via
  the same `groundhog_dev_getAccounts` path the rest of the CLI uses.
- Output: human-readable line on success ("returns: 5"), the standard
  JSON envelope under `--json`, the standard error envelope on
  `Abort`/`Fault`.

`groundhog call --dry-run` (already implemented) is **not** the same
thing and stays as-is: it builds and signs a transaction without
submitting it. It does not execute. `view` actually executes.

## Write path: return data on receipts

### API change

`TransactionReceipt.return_data: Option<HexBytes>` already exists at
`crates/api-types/src/receipt.rs:41`. The CLI already decodes it at
`crates/cli/src/call.rs:79-82`. Both backends currently hard-code
`return_data: None`:

- `crates/gateway/src/handlers/receipt.rs:79`
- `crates/node-supervisor/src/read_backend.rs:137`

The receipt change is purely "wire it up once hogd ships the source
data." No public receipt type changes are needed on the Rust side.

For richer error semantics on `Reverted`, we may also surface the abort
buffer through `ExecStatus::Reverted { message }` (which exists today as
`Option<String>` and is also unpopulated). That is a follow-up; see
"Open questions."

### Scope of return data

Only the **root** invocation's output is exposed. Subinvocation outputs
are not surfaced; a contract that wants to expose subcall results must
return them up the call stack itself, the same constraint as in
production today.

## Protocol surface (hogd / protocol-rs)

This is the part that needs hogd-side buy-in before the Rust work can
proceed. Two pieces are needed.

### 1. Root invocation output on committed tx records

Today, `protocol/indexer.x::TxInfo` carries:

```
struct TxInfo {
    BlockHeight height;
    TxEventLog events;
};

struct TxEventLog {
    SHA256Hash proposed_tx_hash;
    ExecResult exec_result;
    TxEmittedEvent events<kMaxEventsPerTransaction>;
};
```

`ExecResult` is a status enum — it does not carry payload. The root
output is captured in hogexec (`SyscallHandler::last_frame_`) but is
discarded before reaching the event server.

Proposed change: extend the committed tx log to carry the root
invocation's `InvocationFrameOutput` (or a clearer wrapper that maps to
the same union). This belongs on `TxEventLog`, not as a synthetic
`TxEmittedEvent`; it is execution metadata, not contract event data.
Conceptually:

```
struct TxEventLog {
    SHA256Hash proposed_tx_hash;
    ExecResult exec_result;
    RootInvocationOutput root_output;     // new
    TxEmittedEvent events<...>;
};

union RootInvocationOutput switch (InvocationFrameStatusCode status_code) {
case Return: ReturnBuffer return_buffer;
case Abort:  ReturnBuffer abort_buffer;
case Fault:  void;
};
```

Index this for every `TxEventLog` record, including root aborts. Root
execution faults should use `Fault`. Outcomes that never produce a
`TxEventLog` today (validation failure, system error, unrecoverable
executor error) remain outside the receipt path.

Per the chat-log discussion, abort payloads are useful enough for
legitimate clients ("why did my call revert?") to outweigh the
indexer-spam concern; if storage growth becomes an issue, abort buffers
can be moved to a separately-pruned tree or a short-window in-memory
buffer at the indexer.

Only the root output is indexed in v1. Full per-frame return output is a
debug/trace concern, not receipt metadata.

### 2. Read-only execution endpoint

The endpoint should live on a hogd service that can reach hogexec and a
committed state snapshot, most likely a scheduler/executor-owned view
service or a new dedicated view service. The indexer should not grow an
executor dependency just to satisfy this call.

Either:

- **(a)** A new RPC that takes the equivalent of `CallViewRequest` and
  runs hogexec in read-only mode against the latest committed snapshot,
  returning the `InvocationFrameOutput`; or
- **(b)** A reuse of an existing simulation/test path (hogexec already
  has a test-FFI path that materializes the return buffer; if it is
  callable from a service-level RPC, that may be the smaller change).

The Rust supervisor's `read_backend.rs` would consume whichever
endpoint hogd settles on. The shape on the Rust side is independent of
that choice.

The JSON-facing `CallViewRequest`, `CallViewResponse`, and
`RootInvocationOutput` types should live in `crates/api-types`; the
gateway and supervisor should map from protocol XDR types into those
RPC-layer types rather than exposing generated XDR unions directly.

### protocol-rs publication

A new protocol-rs publication is required for both pieces (the
`TxEventLog` shape change and the new view-call RPC). The Rust side
cannot build until those crates are out.

## Implementation order

The dependency graph is mostly linear:

1. **Protocol design.** This spec, plus a hogd-side counterpart (or
   amendment to `2026-04-29-hogd-dev-process-contract.md`) covering the
   `TxEventLog` change and the view-call endpoint. Ryan owns
   sign-off.
2. **hogd implementation.** Both pieces: thread the root output to the
   event server / indexer, and add the read-only execution RPC.
3. **protocol-rs bump and publish.**
4. **Rust supervisor and gateway:**
   - `node-supervisor::read_backend` populates `return_data` from new
     `TxInfo`.
   - `gateway/handlers/receipt` populates `return_data` from new
     `TxInfo`.
   - New `groundhog_callView` handler, both in `node-supervisor` and
     `gateway`.
5. **CLI:** new `groundhog view` subcommand. Calldata encoding, return
   decoding, alias resolution, and `--from dev-N` resolution all reuse
   `groundhog call` plumbing.
6. **End-to-end smoke:** the `groundhog new -> build -> deploy -> call
   -> view` loop in `crates/e2e-tests`.

Steps 4 and 5 can interleave; the rest is strictly sequential.

The Rust-side scaffolding (CLI subcommand stub, RPC handler returning
`NotImplemented`, jsonrpsee trait entry) can land before step 3 if the
wire shape is locked in step 1, but that is an optimization that
trades a bit of speculative work for a tighter critical path.

## Out of scope (v1)

- **Wallet `do_action` simulation.** No flag for routing a view through
  a specific wallet. Use `--from <wallet-address>` to simulate the
  wallet's effective msg.sender on the target contract, which covers
  the realistic read scenarios.
- **Simulated auth tokens / capabilities.** Not part of the auth model
  today.
- **Historical view calls.** No `--at-height` / historical state selector
  in v1. The current public `DbGateway` latest-read contract should be
  extended deliberately before view exposes historical execution.
- **Forking / state overrides.** No `eth_call`-style state diff
  parameter. View executes against committed state.
- **Drop-write simulation.** View is read-only execution. A future
  simulation RPC may execute mutating code and discard writes, but that
  is a different tool.
- **Subinvocation traces in the response.** Only root output is
  surfaced. A future debug RPC could expose more.
- **Gas estimation as a first-class output of view.** The view RPC will
  almost certainly run gas as a side effect; whether to expose it as
  `CallViewResponse.gas_used` is an open question, not a v1 requirement.
- **Receipts for `groundhog call` exposing subcall outputs.** Same
  rationale.

## Open questions

- **`Reverted { message }`.** Today `ExecStatus::Reverted.message` is
  `Option<String>` and unpopulated. Once hogd carries the abort buffer
  on `TxEventLog`, do we also decode it into `message`, or keep it raw
  and let CLI/SDK decode? Recommendation: keep the RPC boundary raw. The
  gateway/supervisor generally do not know the contract method spec; the
  CLI/SDK can decode when metadata is available.
- **Method-level view annotation in the contract spec.** `ContractSpec`
  has no view/mutable marker on methods today. If we ever add one, the
  CLI could route automatically (`groundhog call <c> get` -> view RPC
  for `#[view]` methods). This is a separable, larger change and not
  required for v1.
- **Gas cap on view.** Should view calls be capped lower than tx gas
  caps to prevent expensive read DoS? Probably yes. V1 should start with
  a server-configured cap; a request-level override can be added later.
- **Historical state selector.** The eventual shape should probably use
  a committed height selector, but adding this safely requires hogd-side
  support for retaining/selecting historical snapshots.
- **Caching.** Historical view results at a fixed height are
  deterministic and could be cached. Not v1.
- **Naming.** `groundhog_callView` vs `groundhog_view` vs
  `groundhog_simulate` vs `groundhog_call` (with a flag). The first
  matches the existing `getX` / `sendX` verb-noun pattern; the others
  conflict with simulate-tx semantics or are too overloaded.

## Risks

- **Protocol-rs churn.** The Rust workspace has paid for protocol skew
  before. Locking the wire shape in step 1 minimizes rework.
- **Read-only execution path divergence.** If hogd's view-call path
  diverges from its commit path (different gas accounting, different
  storage semantics), users will see read/write discrepancies. The
  protocol spec must require they share a single executor path, with
  read-only invocation semantics and no commit/auth-validation steps.
- **Indexer storage growth from abort buffers.** Bounded by
  `kMaxReturnBufferLen` (1024 bytes) per tx. At expected throughput
  this is small, but worth re-evaluating once the loadgen profiles
  exist.
