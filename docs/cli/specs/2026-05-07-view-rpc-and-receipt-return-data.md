# Receipt Return Data and View RPC

**Date:** 2026-05-07 (revised 2026-05-15)
**Status:** Milestone A (receipt return-data) shipped. Milestone B (view
RPC) shipped on 2026-05-15 under sandboxed-simulation semantics: the
gateway sends no `InvokeFlag` on root view invocations, hogd's
`ViewingModeData` runs the contract against a discarded scratchpad, and
emitted events are captured and surfaced. The original 2026-05-07 draft
scoped view as strict read-only at the syscall layer; that semantic was
reversed once we recognized the wallet-preview use case is simulation,
not pure read. The strict-read-only enforcement work (PR #1959 on hogd)
was closed; only the defense-in-depth `kBlockExecutionModeViewing` half
was kept (hogd #1961). This document has been updated in-place; git
history captures the pre-pivot language.

**Companion to:**

- [`../roadmap.md`](../roadmap.md) - CLI roadmap; this spec defines the
  Phase-2 follow-up that unblocks `groundhog call <c> get` returning a
  decoded value.
- [`2026-05-13-view-rpc-protocol-rs-follow-up.md`](2026-05-13-view-rpc-protocol-rs-follow-up.md)
  - post-hogd-PR-1902 protocol/protocol-rs service contract needed to
  wire Milestone B to a real backend.

## Goal

Close the contract-read DX gap without creating a local-dev-only API:

1. A user submitting a state-changing transaction can see the root
   contract return value on the resulting receipt, without re-executing
   or encoding the result as an event.
2. A user can later call a getter on a deployed contract without holding
   a key, funding an account, signing a transaction, or writing to chain
   history.

The motivating CLI loop:

```text
groundhog new my-counter && cd my-counter
groundhog build
groundhog node          # in another terminal
groundhog deploy        # writes alias my-counter -> address
groundhog call my-counter increment
groundhog view my-counter get   # -> 1
```

Today, `groundhog call` cannot decode most committed call results
because receipts omit return data, and `groundhog view` does not exist.
Those are related but not identical hogd asks, so this spec treats them
as separate milestones.

## Non-Goals

This must not be a Rust-only or localhost-only shortcut. The production
gateway and `groundhog node` node-supervisor both serve the shared
`GroundhogApi` JSON-RPC surface. `hogd-dev` should remain a supervisor-
shaped local composition of real hogd services, not a divergent semantic
implementation for DX convenience.

Concretely:

- The gateway and node-supervisor should expose the same public method
  names, request shapes, response shapes, and error semantics.
- node-supervisor should consume hogd-dev-advertised service endpoints
  for receipt return data and view execution, just as the real gateway
  consumes configured hogd service endpoints.
- Rust may add scaffolding only after the hogd/protocol shape is locked.
  It should not fake successful view execution or receipt return-data
  locally while hogd still lacks the source data.

## Why Both Halves

Comparable chains consistently expose a non-mutating contract execution path,
but they vary on whether committed transaction metadata carries return data:

| Chain | Read RPC | Write result surface |
|---|---|---|
| Ethereum | `eth_call` | Receipts do not include contract return data |
| Sui | `sui_devInspectTransactionBlock` | Dev inspect includes return values; committed effects do not model a root return value |
| Aptos | `POST /view` (Move `#[view]`) | Transaction responses do not expose ordinary function returns |
| NEAR | `query` / view functions | Execution outcomes can carry `SuccessValue` |
| CosmWasm | smart query | Execute responses can carry optional `data`, though events are the common app surface |
| Solana | `simulateTransaction` | Confirmed transaction metadata can include `meta.returnData` |

The DX argument for a dedicated contract preview path is broad: previews
should not require a key, should not commit, should not pollute history,
and should eventually be runnable against historical heights. Receipt
return-data alone does not satisfy any of those.

The DX argument for committed return-data is narrower but immediate:
when a state-changing call has a meaningful return value, forcing
callers to re-execute or encode that value as an event is hostile.

The two halves share one concept - exporting the root
`InvocationFrameOutput` - but they do not share the same hogd lift:

- **Receipt return-data** threads root output into the event-server /
  indexer write path and bumps the committed tx metadata shape.
- **View RPC** adds a top-level simulation execution entry point,
  service endpoint, and gateway/node-supervisor routing.

They should be sequenced separately. Receipt return-data is the immediate
PR 5 dependency because it unblocks `groundhog call ...` output with the
smallest cross-team surface. View RPC is strategically important, but it
is a larger follow-up.

### Sequencing Risk

Shipping Milestone A long before Milestone B carries DX risk. If "read
state via `groundhog call`" lands first and gets baked into onboarding
docs, users will learn the wrong primary read pattern. That pattern -
read by submitting and committing a transaction - is worse DX than
Ethereum, NEAR, Aptos, Sui, and CosmWasm offer for reads, and re-creates
the dev-account-on-testnet/mainnet dependency that Non-Goals explicitly
avoids.

Mitigation: Milestone A's CLI surface should be framed as a write-path
enhancement (a `transfer` returning the new balance, an admin op
returning a config version), not as the canonical read path. The CLI
help text, README examples, and onboarding docs should steer users
toward `groundhog view` for reads once Milestone B ships. If Milestone B
is significantly delayed, the wire-shape change can still land in M-A
without promoting `groundhog call`-as-read in onboarding.

## End-User Call Patterns

The CLI and a wallet application would consume the same RPC surface but
exercise it differently. Aligning on the same shape now keeps both
audiences served without growing divergent endpoints later.

| Workflow stage | CLI | Wallet app |
|---|---|---|
| Inspect account state | `getStorageAt`, plus `view` for computed getters | Same; storage reads dominate, view fills in computed values |
| Preview a pending tx | Not a primary CLI need; users re-run locally | Critical - "if I sign this, what happens?" is the most-requested wallet feature on every chain |
| Sign and submit | `groundhog call`, `groundhog deploy` | `sendRawTransaction` |
| Confirm an outcome | Receipt + decoded return | Receipt + decoded return |

Milestone B's view RPC with the optional `caller` field covers most
wallet-preview workflows. A wallet building a transaction can simulate
the inner contract call as if its own wallet contract were the immediate
caller, by passing `caller = <wallet-address>`. The wallet's own
`do_action` business logic is not simulated, but the target contract's
return/abort behavior, root return value, and emitted events are.

Cases where the wallet's own `do_action` materially changes the outer
behavior (custom auth gates, multi-action wallets, replay protections
that change resulting calldata) are not covered by Milestone B and
remain in the deferred "wallet `do_action` simulation" follow-up.
That is a narrow category, not "wallet preview" as a whole.

This alignment has two concrete consequences for Milestone B's shape:

- `ViewResponse.events` carries the events the simulated frame emitted,
  so wallet-preview callers see what events a real submit would
  produce. Hogd captures these from `get_most_recent_frame_events()`.
- The `caller` semantics must be stable enough for wallet code to rely
  on. A wallet sending `caller = <its own wallet address>` should see
  exactly what the inner contract sees during a real `do_action`
  subinvocation; the only difference is the absence of the outer
  wallet's own auth/business-logic frame.

## Milestone A: Receipt Return Data

### Rust API State

`TransactionReceipt.return_data: Option<HexBytes>` already exists at
`crates/api-types/src/receipt.rs:41`. The CLI already attempts to decode
it at `crates/cli/src/call.rs:79-82`. Both real read backends currently
hard-code `return_data: None`:

- `crates/gateway/src/handlers/receipt.rs:79`
- `crates/node-supervisor/src/read_backend.rs:137`

The Rust receipt type does not need a public shape change for the return
case. Once hogd indexes root output, both backends should map
`RootInvocationOutput::Return { return_buffer }` into
`TransactionReceipt.return_data`.

Abort output should still be indexed on the hogd side. Recommendation
for M-A v1: surface raw abort bytes on `TransactionReceipt` (e.g., a new
`abort_data: Option<HexBytes>` field) alongside `return_data`. The wire
change is the same one M-A already requires; the marginal Rust API
surface is small. Without it, M-A leaves users staring at
`status: reverted` with no payload, which is worse than what Ethereum
exposes for revert debugging today. Decoding raw bytes into
`ExecStatus::Reverted { message }` against contract-spec metadata
remains a follow-up; the CLI/SDK can do that decoding when metadata is
available, while the gateway/supervisor stays raw at the RPC boundary.

### Scope

Only the root invocation's output is exposed. Subinvocation outputs are
not receipt metadata; a contract that wants to expose a subcall result
must return it up the call stack itself.

Mapping from root output to receipt fields:

- `Return { return_buffer }` -> `return_data = Some(...)`,
  `abort_data = None`.
- `Abort { abort_buffer }` -> `return_data = None`,
  `abort_data = Some(...)`.
- `Fault` -> both fields `None`; status alone signals the fault.

### Protocol Surface

Today, `protocol/indexer.x::TxInfo` carries:

```xdr
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

`ExecResult` is a status enum; it does not carry payload. Hogexec already
has the root output available through `SyscallHandler::last_frame_` and
`get_most_recent_frame_output()`, but `emit_events()` currently sends
only hash, exec result, and emitted contract events.

Proposed committed-log shape:

```xdr
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

This belongs on `TxEventLog`, not as a synthetic `TxEmittedEvent`;
it is execution metadata, not contract event data.

Index this for every `TxEventLog` record that exists today, including
root aborts. Root execution faults should use `Fault`. Outcomes that
never produce `TxEventLog` today, such as validation failure, system
error, or unrecoverable executor error, remain outside the receipt path
unless hogd deliberately expands which outcomes emit tx logs.

Per the chat-log discussion, abort payloads are useful enough for
legitimate clients to outweigh the indexer-spam concern. If storage
growth becomes an issue, abort buffers can move to a separately-pruned
tree or a short-window in-memory buffer.

### Indexer Migration

Adding a field to `TxEventLog` is an XDR format change. The current
indexer stores `BlockHeight || x23::serialize(TxEventLog)` in RocksDB
for the tx index, so existing rows will not naturally decode as the new
struct.

hogd should choose an explicit migration strategy before protocol-rs is
published. Viable shapes:

- Add a versioned tx-index value wrapper and decode old rows separately.
- Store `root_output` in a sibling keyed record/column family keyed by
  transaction hash, leaving historical `TxEventLog` values intact.
- Teach `IndexDB::lookup_tx` to attempt the new decode first, then fall
  back to the old `TxEventLog` shape and surface "output unavailable"
  to the gateway for old rows. Do not synthesize `Fault` for old rows;
  that would misrepresent historical execution.

The public gateway can tolerate old rows without root output because
both `TransactionReceipt.return_data` and `abort_data` are optional.
New rows emitted by the updated event-server path should carry root
output.

### Hogd Questions

Ryan/hogd needs to decide:

- Should `root_output` reuse `InvocationFrameOutput` directly, or use a
  committed-log wrapper such as `RootInvocationOutput`?
- Does the field live directly on `TxEventLog`, or in a sibling
  tx-output table to simplify old-row compatibility?
- Are abort buffers indexed for all committed root aborts? Recommendation:
  yes.
- What output is recorded for deterministic faults that still emit a
  `TxEventLog`? Recommendation: `Fault`.
- What is the old-row migration/default story?
- Does the event-server/indexer API version change in place, or does hogd
  need a parallel version during rollout?

### Milestone A Implementation Order

1. protocol/hogd design sign-off for `TxEventLog` root output and
   indexer migration.
2. hogd implementation: populate root output in the event-server /
   indexer path.
3. protocol-rs bump and publish.
4. Rust API + mapping:
   - add `abort_data: Option<HexBytes>` to `TransactionReceipt` in
     `crates/api-types`;
   - `gateway/handlers/receipt` maps `Return` -> `return_data`,
     `Abort` -> `abort_data`, `Fault` -> neither;
   - `node-supervisor::read_backend` does the same against hogd-dev's
     `IndexerBlockExplorer` endpoint;
   - shared conversion helpers live in `api-types` or `rpc-server` if
     both backends need the same XDR mapping;
   - update OpenRPC and TypeScript codegen artifacts to reflect the new
     receipt field.
5. CLI: `groundhog call <contract> <method>` prints the decoded return
   when local contract metadata is available, raw return hex when bytes
   exist but no metadata can decode them, and raw abort hex on
   `Reverted` when `abort_data` is non-empty.

## Milestone B: View RPC and `groundhog view`

### RPC Surface

```rust
#[method(name = "view")]
async fn view(&self, params: ViewRequest) -> RpcResult<ViewResponse>;

pub struct ViewRequest {
    pub contract:   ContractAddress,
    pub entrypoint: HexBytes,                // exactly 8-byte ContractEntrypointID
    pub calldata:   HexBytes,
    pub caller:     Option<ContractAddress>, // optional simulated msg.sender
}

pub struct ViewResponse {
    pub output:     RootInvocationOutput,
    pub events:     Vec<EmittedEvent>,       // captured from the simulated frame
    pub height:     BlockHeight,             // height the read executed against
}
```

Method name follows the existing `groundhog_` prefix; `groundhog_view`
is the wire name. The compound `groundhog_callView` was rejected: every
modern non-EVM chain that exposes a view-style RPC uses a single
short verb (`query` on NEAR, `view` on Aptos, `eth_call` on Ethereum).
A noun-shaped method also avoids reading as "call a view," which is
not what the operation does.

The JSON-facing `ViewRequest`, `ViewResponse`, and `RootInvocationOutput`
types should live in `crates/api-types`. The gateway and node-supervisor
should map from protocol XDR types into RPC-layer types rather than
exposing generated XDR unions directly.

Adding this method touches the shared Rust RPC surface:

- `crates/rpc-api` trait entry
- `groundhog-rpc-client` generated typed client
- `rpc-server` implementation and backend plane
- gateway backend adapter
- node-supervisor backend adapter
- OpenRPC snapshot/artifact
- TypeScript codegen output

### Semantics

- No transaction is constructed, signed, committed, or charged.
- No `AuthClaim` is validated.
- Storage reads are served from the latest committed snapshot. Historical
  view calls are out of scope for v1 because the current public
  `DbGateway` explicitly exposes latest-committed reads only.
- Execution runs the contract under hogd's `ViewingModeData` sandbox:
  state-modifying syscalls succeed against a scratchpad that the view
  path never commits, and emitted events are captured and surfaced on
  the response. A state-mutating entrypoint like `decrement` runs to
  completion, returns its declared output, and leaves committed storage
  unchanged. Wallet-preview callers see the events a real submit would
  produce.
- The gateway passes `invoke_flags = InvokeFlag::None` on the root
  invocation, matching the protocol convention that "the root
  transaction invocation always has no flags set" (`invoke_flag.x`).
  Sandboxing is a property of the execution mode, not of a per-frame
  read-only flag.
- Gas accounting runs as in production under
  `RpcServerConfig::view_gas_limit`, but no gas is charged. Gas-cap
  exhaustion is a deterministic execution outcome: it surfaces as
  `RootInvocationOutput::Fault`, not as a JSON-RPC error. This mirrors
  how `ExecResult::ExecResultTxDeterministicError` flows through the
  receipt path on the commit side.
- View execution is pinned to a single committed snapshot for its full
  duration. `ViewResponse.height` is the height of that snapshot. A
  view that races a new block sees only the snapshot it started on;
  it does not observe newer state mid-execution.
- Return shape mirrors root invocation output: `Return`, `Abort`, or
  `Fault`. All three are RPC success responses carrying the response
  body. Only transport-level failures and invariant violations surface
  as JSON-RPC errors. Request validation failures (e.g. `entrypoint`
  not exactly 8 bytes, malformed hex, contract address not a valid
  `ContractAddress`) surface as JSON-RPC `invalid params` (-32602)
  before any execution begins. Abort and abort-buffer bytes must reach
  the client; routing them through error envelopes loses the bytes.
- Contracts can introspect that they are running under view via
  `get_block_execution_mode() == kBlockExecutionModeViewing` (hogd
  #1949 / #1961). A contract that branches on this can return one value
  in view and a different value in commit; wallet-preview faithfulness
  depends on contract authors not doing that. This is the same caveat
  every chain with a `simulate`-style RPC carries.

### Supervisor Behavior When ViewGateway Endpoint Is Absent

`hogd-dev` may be released before its `ViewGateway` endpoint is
implemented, the same way it shipped without an `IndexerBlockExplorer` endpoint
initially. node-supervisor should treat `endpoints.view_gateway` as
optional in readiness, exactly as it treats other optional x23 services:
an absent endpoint leaves the public `groundhog_view` method returning
`NotImplemented`, not a startup failure. The gateway's production
configuration follows the same rule against real hogd.

### Caller Context

Groundhog's current `get_msg_sender` behavior is frame-stack based:

- A subinvocation sees the previous invocation frame's address.
- A root invocation has no previous frame.
- Calling `get_msg_sender` from a root frame returns
  `kSyscallErrorCodeIsRootTransaction`.

Therefore view calls need two explicit modes:

- `caller = None`: execute the target as a root frame. Methods that do
  not read `msg.sender`, such as `get_balance_of(addr)`, work normally.
  Methods that read `msg.sender`, such as `get_balance()`, fail or abort
  according to normal contract behavior.
- `caller = Some(addr)`: execute an unsigned simulated invocation where
  the target observes `msg.sender = addr`.

The second mode is a service-level execution mode, not a transaction
shape. Passing `caller` to an ordinary root invocation is insufficient.
hogd must either create a synthetic parent-frame context whose address is
`caller`, then start the target as a subinvocation under viewing mode,
or provide an equivalent executor hook that makes `get_msg_sender`
return `caller` for the view frame.

This is caller context only. It is not proof of ownership, not a wallet
action, not an `AuthClaim`, and not a capability grant.

### Existing Hogexec Primitive

hogd has a top-level viewing mode (`ViewingModeData`) and an executor
`view_transaction` primitive (hogd PR #1902). Under viewing mode:

- Writes go to a per-transaction scratchpad that the view path never
  commits.
- `log_event` is permitted and captured; `view.cc` copies emitted
  events into the `ViewGatewayResult` payload via
  `get_most_recent_frame_events()`.
- `get_block_execution_mode()` returns `kBlockExecutionModeViewing`
  (hogd #1949 / #1961), giving hogdb a defense-in-depth signal that
  writes from this execution must not be committed.

`InvokeFlag::ReadOnly` (a separate, per-frame read-only flag with
syscall-layer enforcement of writes) is still available for
subinvocations that explicitly want strict read-only semantics inside
a real transaction. View is not that use case; the gateway passes no
flag on the root invocation and relies on viewing-mode sandboxing.

Recommendation: implement a dedicated service-level `ViewGateway`
endpoint. The hogexec test-FFI path can inform the implementation, but
should not be the production/dev-node API surface.

### Endpoint Placement

The endpoint should live on a hogd service that can reach hogexec and a
committed state snapshot, most likely a scheduler/executor-owned view
service or a dedicated view service. The indexer should not grow an
executor dependency just to satisfy view calls.

Both gateway and node-supervisor should consume that service shape:

- The real gateway uses a configured x23 endpoint.
- hogd-dev advertises the same endpoint in readiness.
- node-supervisor connects to that endpoint and exposes the same
  `groundhog_view` behavior through the shared `GroundhogApiServer`.

### CLI Grammar

```text
groundhog view <contract> <method> [args...]
groundhog view <contract> <method> [args...] --from <addr-or-dev-N>
groundhog view <contract> 0x<entrypoint> --calldata 0x<bytes>
groundhog view <contract> <method> [args...] --json
```

- `<contract>` resolves through the same alias table `groundhog call`
  uses (`groundhog.toml` per-profile aliases), or can be an explicit
  address.
- Named `<method>` calls require local/project contract metadata in v1.
  When metadata is available, CLI encodes args and decodes the return
  value.
- Without metadata, CLI should require an explicit 8-byte entrypoint and
  raw calldata, then print raw return hex. Fetching embedded contract
  specs from deployed bytecode/address is a future feature and requires a
  hogd/gateway read surface for deployed code or spec metadata.
- `--from <hex-address>` works anywhere.
- `--from dev-N` is local-dev sugar resolved through
  `groundhog_dev_getAccounts`. On endpoints that do not expose
  `groundhog_dev_getAccounts`, the CLI should return a clear error and
  ask for an explicit address.
- Output: human-readable line on `Return` (decoded if metadata is
  present, raw hex otherwise) plus any non-empty events list; on
  `Abort`, a clear "view aborted" line with raw abort hex; on `Fault`,
  a clear fault line. The JSON envelope under `--json` mirrors the
  `ViewResponse` shape in all three cases. JSON-RPC error envelopes are
  reserved for transport/backend availability, request validation, and
  gateway invariant failures.

`groundhog call --dry-run` is not the same thing and stays as-is: it
builds and signs a transaction without submitting it. It does not execute.
`view` actually executes the contract under sandboxed simulation.

### Auth Simulation

Wallet `do_action` simulation and simulated auth tokens are out of scope
for v1. This is a narrow deferral, not a deferral of "wallet preview"
as a category.

For most wallet-preview workflows that depend on target return/abort
behavior, Milestone B is sufficient: the wallet simulates the inner
contract call with `caller = <its own wallet address>`, and the target
contract's return/abort behavior, return value, and emitted events are
identical to what a real `do_action` subinvocation would observe. The
missing piece is the wallet's own outer-frame business logic.

Supporting full `do_action` simulation requires a skip-auth or
simulated-auth mode in the executor. That is security-relevant: if such
a mode were ever reachable from a commit path, it would let unsigned
transactions be committed. If a future milestone delivers it, it should
be a separate executor entry point with its own service endpoint, not
smuggled into this view RPC.

### Milestone B Hogd Follow-Up

`ViewingModeData` already provides scratchpad-discarded writes and event
capture, so public v1 ships as full sandboxed simulation. The only
known hogd follow-up is rate-limiting and per-tenant fairness once
view traffic is exposed to untrusted clients (see Risks). Defense-in-
depth in hogdb keys on `kBlockExecutionModeViewing`; that signal is
load-bearing for "writes from this execution must not commit," not
optional decoration.

### Milestone B Implementation Order

1. hogd/protocol design sign-off for the `ViewGateway` endpoint shape, caller
   semantics, height threading, and event-capture behavior.
2. hogd implementation: top-level view execution entry point using
   latest committed state, optional caller context, gas cap, sandboxed
   writes, captured events, and root output response.
3. protocol-rs bump and publish, including the follow-up in
   [`2026-05-13-view-rpc-protocol-rs-follow-up.md`](2026-05-13-view-rpc-protocol-rs-follow-up.md).
4. Rust API:
   - add `ViewRequest`, `ViewResponse`, and output enum to `api-types`;
   - add `groundhog_view` to `rpc-api`;
   - add a dedicated `ViewPlane` or equivalent backend trait in
     `rpc-server`;
   - wire gateway and node-supervisor adapters to hogd service clients;
   - update OpenRPC and TypeScript codegen artifacts.
5. CLI: add `groundhog view`, reusing alias resolution, calldata
   encoding, return decoding, and `dev-N` account lookup where
   applicable.
6. End-to-end smoke: `groundhog new -> build -> deploy -> call -> view`
   in `crates/e2e-tests`.
7. Bench follow-up: add `groundhog_view` to `crates/bench` once the
   endpoint is stable.

## Test Plan

### Receipt Return-Data

- Unit test XDR-to-API mapping for `Return`, `Abort`, and `Fault`.
- Gateway handler test: `TxInfo` with root `Return` populates
  `TransactionReceipt.return_data`.
- Gateway handler test: `TxInfo` with root `Abort` populates
  `TransactionReceipt.abort_data`; `return_data` stays `None`.
- Gateway handler test: `TxInfo` with root `Fault` leaves both
  `return_data` and `abort_data` `None`.
- node-supervisor read-backend test: same mapping against the hogd-dev
  indexer client type.
- Old-row/migration test on hogd side: historical index rows do not break
  `get_transaction_info`; old rows surface with both fields `None`.
- CLI test: receipt return bytes decode with local metadata, and print
  raw hex when metadata is missing.
- CLI test: receipt abort bytes print as raw hex on `groundhog call`
  when the call reverts.

### View RPC

- A pure-read method returns bytes and height; events list reflects any
  events the contract emitted (typically empty for getter-style methods).
- An event-emitting entrypoint surfaces emitted events on
  `ViewResponse.events` with the contract's address and topics
  populated. The `scripts/smoke-local-dx.sh` `view ping` step is the
  reference assertion.
- A state-mutating entrypoint (`decrement`) runs to completion under
  view, returns `Return`, and leaves committed storage unchanged. The
  post-view storage check is the authoritative "didn't commit"
  guarantee.
- Abort response is returned as RPC success with raw abort output.
- Fault response is returned as RPC success with the `Fault` variant.
- Root-frame `get_msg_sender` view with `caller = None` fails normally.
- The same method with `caller = Some(addr)` observes that address as
  `msg.sender` and otherwise behaves as a real subinvocation.
- Gas cap is enforced and surfaces as
  `ViewResponse.output = Fault` (RPC success).
- View execution sees a single snapshot: a contemporaneous commit on a
  parallel chain-driver path does not change the state observed by an
  in-flight view, and `ViewResponse.height` matches the snapshot the
  view actually executed against.
- Request validation: `entrypoint` not exactly 8 bytes, malformed hex,
  or invalid `ContractAddress` returns JSON-RPC `invalid params`
  before execution.
- `--from dev-N` resolves through `groundhog_dev_getAccounts` only on
  local dev endpoints; off-dev endpoints surface a clear error.
- Supervisor conformance: when hogd-dev readiness omits the view
  endpoint, `groundhog_view` returns `NotImplemented` instead of failing
  startup.
- Gateway and node-supervisor conformance tests exercise the same public
  JSON-RPC shape.
- OpenRPC snapshot and TypeScript codegen include the new method and
  types.

## Out of Scope

- Historical view calls. No `--at-height` / historical state selector in
  v1.
- Forking / state overrides. No `eth_call`-style state diff parameter.
- Wallet `do_action` simulation.
- Simulated auth tokens / capabilities.
- Subinvocation traces in receipt or view responses.
- Gas estimation as a first-class view output.
- Receipts exposing subcall outputs.
- Fetching a deployed contract's embedded spec by address.

## Open Questions

- **Receipt abort decoding into `ExecStatus::Reverted { message }`.**
  M-A v1 recommendation is to expose raw abort bytes as `abort_data` on
  the receipt; whether to additionally decode them into a structured
  `Reverted { message }` field at the gateway/supervisor boundary is a
  follow-up. Recommendation: keep the gateway raw and let CLI/SDK decode
  with contract-spec metadata when available.
- **Method-level mutability annotation in the contract spec.** `ContractSpec`
  has no view/mutable marker today. Under sandboxed simulation, this
  annotation is no longer required for Milestone B routing, but it still
  matters for UX: the CLI/SDK can warn when a user simulates a method that
  is known to be state-changing, or when a user submits a transaction for a
  method that is known to be preview-only. Until the annotation exists,
  users choose `view` vs `call` explicitly.
- **View gas cap.** V1 should start with
  `RpcServerConfig::view_gas_limit`. Concrete backends should set a
  bounded production value before exposing public view support; an
  unbounded scaffold default is only acceptable while the method returns
  `NotImplemented`. A request-level override can be added later if
  needed.
- **Historical state selector.** The eventual shape should probably use
  a committed height selector, but that requires hogd-side support for
  retaining/selecting historical snapshots.
- **Caching.** Historical view results at a fixed height are
  deterministic and could be cached. Not v1.

## Risks

- **Under-scoping hogd work.** Receipt return-data and view RPC should
  not be budgeted as one small change. They are separate milestones.
- **Protocol-rs churn.** Lock the protocol shape before Rust scaffolding.
- **Indexer migration.** `TxEventLog` is serialized into RocksDB today;
  the migration/default story must be explicit.
- **Simulation divergence from commit.** View must share hogexec
  semantics with commit execution, with only the write-scratchpad,
  no-commit, and no-auth-validation differences. A bug that lets
  view return a different value than the same call would on commit
  (other than the writes themselves) is a wallet-preview correctness
  hole. The `kBlockExecutionModeViewing` syscall return value is
  observable to contracts, which can in principle branch on it; that's
  a contract-author concern, not a host guarantee.
- **Security boundary for auth simulation.** Any future skip-auth mode
  must be isolated from commit paths.
- **Indexer storage growth from abort buffers.** Bounded by
  `kMaxReturnBufferLen` (1024 bytes) per tx, but worth re-evaluating once
  loadgen profiles exist.
- **View execution contention.** View invocations share hogexec with
  proposing/validating execution. Heavy view traffic could compete with
  commit-path execution. Mitigation in v1 is the server-configured gas
  cap; longer term, per-tenant or per-IP rate limiting and a separate
  executor pool for view traffic should be considered.
- **Unauthenticated, free RPC.** View calls are unsigned and not
  metered. The public gateway should rate-limit view requests to prevent
  amplification attacks. Concrete limits are out of scope for this spec
  but should land before view is exposed to untrusted clients.
