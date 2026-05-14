# View RPC Protocol-RS Follow-Up

**Date:** 2026-05-13
**Status:** Draft follow-up after hogd PR #1902 merged as `7b659115`.

**Companion to:**

- [`2026-05-07-view-rpc-and-receipt-return-data.md`](2026-05-07-view-rpc-and-receipt-return-data.md)
  - public JSON-RPC and CLI shape.

## Context

`groundhog-rpc` now has the public `groundhog_view` scaffold:

- `ViewRequest { contract, entrypoint, calldata, caller }`
- `ViewResponse { output, events, height }`
- `RootInvocationOutput::{Return, Abort, Fault}`
- shared request validation, including 8-byte entrypoints and a calldata
  cap
- a backend default of `NotImplemented` until hogd exposes a real view
  service

hogd PR #1902 landed the executor primitive that makes this possible:

- `protocol/view.x` defines `TransactionViewQuery`,
  `TransactionViewResult`, `TransactionViewStatus`, and
  `ExpectedViewResult`.
- hogexec has `ExecutorContext::view_transaction(...)`.
- hogexec tests exercise the view execution path.

That is the right lower layer, but it is not yet a consumable
`groundhog-rpc` backend contract. As of the merge:

- `protocol/view.x` defines data types only; it does not define an x23
  `program`.
- `protocol-rs/scripts/generate.sh` does not generate `view.x`.
- `protocol-rs` does not publish a view RPC client crate/module.
- `hogd-dev` readiness does not advertise a `view_gateway` endpoint.
- The executor result does not carry the committed snapshot height that
  `ViewResponse.height` requires.

This spec defines the smallest cross-repo follow-up that lets
`groundhog-rpc` replace `NotImplemented` with a real adapter.

## Goals

1. Publish Rust bindings for the view XDR data types.
2. Publish a generated Rust client for an externally reachable view
   service.
3. Ensure the service result carries the exact committed height used for
   execution.
4. Keep the public JSON-RPC shape from the 2026-05-07 spec unchanged.
5. Keep deterministic execution outcomes in successful JSON-RPC response
   bodies; reserve JSON-RPC errors for transport, overload, timeout,
   backend availability, and request validation.

## Non-Goals

- No public JSON-RPC request shape change (no users/clients yet)
- No historical view selector.
- No wallet `do_action` simulation.
- No simulated auth tokens or skip-auth execution mode.
- No request-level gas override.
- No local `groundhog-rpc` fake execution path.

## Required Protocol Shape

### View Data Generation

`protocol-rs` should generate `protocol/view.x` into
`groundhog-xdr/src/view.rs` and re-export it from `groundhog-xdr`.

This gives `groundhog-rpc` stable Rust names for:

- `TransactionViewQuery`
- `TransactionViewResult`
- `TransactionViewStatus`

The raw `ExpectedViewResult` executor union will be generated as part of
`view.x`, but `groundhog-rpc` should not depend on it directly once a
service wrapper exists. It is the hogexec internal/test shape, not the
gateway contract.

Generating these types is necessary but not sufficient: the raw executor
result still lacks the snapshot height needed by public JSON-RPC.

### Service-Level Result

The external service consumed by `groundhog-rpc` must return the height
of the committed snapshot used for the view. `groundhog-rpc` must not
infer this with a separate `get_tip` call, because the tip can advance
between calls and report the wrong height.

The missing height in hogd PR #1902 is an implementation omission, not a
planned public protocol limitation. `ViewResponse.height` stays required
for v1.

The raw executor primitive is not the final public service boundary. The
view service should add a thin retry layer above
`ExecutorContext::view_transaction(...)`:

1. Read the current committed `DbTip` and construct the view snapshot
   from that tip.
2. Run `view_transaction` against that snapshot.
3. If the raw result is `Abandoned`, interpret that as hogdb having moved
   ahead while the attempt was using its selected snapshot, refresh to a
   later committed height, and retry up to a bounded `N` attempts.
4. On success, return the height from the snapshot that actually
   executed.
5. Return `ViewGatewayAbandoned` only after the retry budget is
   exhausted.

Recommended wrapper:

```c
struct ViewGatewayPayload {
  // Height of the committed snapshot used for the full view execution.
  BlockHeight height;

  // Root invocation output after mapping executor pre-frame
  // deterministic failures to Fault.
  InvocationFrameOutput output;

  // Empty for public v1 if view remains strict read-only. Can carry
  // ephemeral view events later if hogd adds view-event recording that
  // does not permit storage mutation.
  TxEmittedEvent events<>;
};

enum ViewGatewayOutcome {
  ViewGatewayOk = 0,

  // Raw view attempts returned Abandoned until the bounded service retry
  // budget was exhausted. This is a real hogexec outcome: the view
  // primitive maps host SystemErrorCode::Abandon to
  // TransactionViewStatus::Abandoned. The service retries it internally
  // against a later committed snapshot before surfacing this outcome.
  ViewGatewayAbandoned = 1,

  // Server-side invariant failure or unavailable execution dependency.
  ViewGatewayInternalError = 2
};

union ViewGatewayResult switch (ViewGatewayOutcome outcome) {
case ViewGatewayOk:
  ViewGatewayPayload payload;
default:
  void;
};
```

This wrapper intentionally differs from raw `ExpectedViewResult`:

- `ViewGatewayOk` is the only path that maps to a public
  `ViewResponse`.
- `ViewGatewayAbandoned` maps to JSON-RPC `BACKEND_UNAVAILABLE` with a
  retry-oriented message after the hogd view service exhausts its own
  retry budget.
- `ViewGatewayInternalError` maps to JSON-RPC `BACKEND_UNAVAILABLE`.
- Raw executor `InvalidInput(error_code)` should be mapped by the hogd
  service to `ViewGatewayOk` with `InvocationFrameOutput::Fault`, unless
  the public JSON-RPC `Fault` variant is expanded before it stabilizes.
- Raw executor `NondeterministicDataValidationFailure` should not occur
  for v1 public view requests because the public request supplies no
  nondeterministic data. If it does occur, treat it as an internal
  backend failure for v1.

The important invariant is honesty: a successful public view response
carries the exact height of the snapshot actually executed. It must not
carry a post-view `get_tip` result. If a backend cannot return the exact
height, it is not ready to back public `groundhog_view` and should leave
the JSON-RPC method as `NotImplemented`.

Implementation note for hogd: the view service should prime the
`StorageSnapshotCache` by reading its `DbTip` before invoking the
contract, then reuse that cached `DbTip.height` in the response for that
attempt. Calling `get_tip` after `view_transaction` has the same race as
doing it in `groundhog-rpc`. The retry budget should live in hogd service
configuration or a reviewed constant; it is not a JSON-RPC request
parameter.

### Service Program

Add an x23 program for the view service. The exact file name can be
`view_gateway.x`, `view_rpc.x`, or another reviewed name; the generated
crate/module should be stable before publishing.

Recommended shape:

```c
%#include "network.hh"
%#include "types.hh"
%#include "view.hh"

namespace hogd {

enum ViewGatewayIntroOutcome {
  ViewGatewayIntroSuccess = 0,
  ViewGatewayIntroMismatchedProtocol = 1
};

program ViewGateway
{
  version ViewGateway1
  {
    ViewGatewayIntroOutcome viewgateway_intro(Protocol) = 0;

    // Execute a read-only invocation against one committed snapshot.
    ViewGatewayResult viewgateway_view(TransactionViewQuery) = 1;
  } = 1;
}
= 0x56475759; // proposed "VGWY"; final protocol PR owns this number

} // namespace hogd
```

The method name should use the same generated-client style as the
existing gateway-facing programs:

- `schedgateway_*` for `SchedGateway`
- `dbgateway_*` for `DbGateway`
- `viewgateway_*` for `ViewGateway`

### Query Semantics

The public `ViewRequest` maps into `TransactionViewQuery` as follows:

| JSON-RPC field | XDR field |
|---|---|
| `contract` | `invocation.invoked_address` |
| `entrypoint` | `invocation.entrypoint` |
| `calldata` | `invocation.calldata` |
| `RpcServerConfig::view_gas_limit` | `invocation.gas_limit` |
| `caller = None` | `parent_address = nullptr` |
| `caller = Some(addr)` | `parent_address = &addr` |

The scaffold default for `RpcServerConfig::view_gas_limit` is
`u64::MAX`, matching the existing "not configured" convention. Concrete
gateway and node-supervisor view backends must override that default with
a bounded production value before advertising public view support.

The remaining `TransactionViewQuery` fields need explicit protocol
documentation before protocol-rs publishes them:

- `invoke_flags` should include `InvokeFlag::ReadOnly` and should not
  include `TryInvoke` or `DelegateCall` for v1 public view.
- `forwarded_tokens` should be empty.
- `auth_claim` must not be validated for v1 public view. The preferred
  protocol shape is `AuthClaim *auth_claim`, with `nullptr` for public
  view. A canonical ignored sentinel, such as a zero-weight
  source-address claim, is acceptable only if hogd rejects the optional
  field for compatibility reasons.

The hogd service should enforce this public subset at its boundary:
either reject non-empty `forwarded_tokens` and non-`ReadOnly` /
non-empty-disallowed flags, or normalize them before execution. It must
not silently honor `TryInvoke`, `DelegateCall`, auth claims, or forwarded
tokens from arbitrary callers of `viewgateway_view`.

The important invariant: public view is caller context, not authorization
simulation. Passing `caller` must affect `get_msg_sender`; it must not
prove ownership or grant capabilities.

Because public v1 sends `InvokeFlag::ReadOnly`, emitted events are not
part of v1 public view. The current hogexec read-only path rejects
`log_event`, so `ViewGatewayPayload.events` should be empty. If wallet
preview events become a v1 requirement, hogd must add a distinct
view-event mode that records events while still rejecting storage writes;
`groundhog-rpc` should not get events by switching to drop-write
simulation.

## Required Protocol-RS Changes

`protocol-rs` should grow a first-class view binding modeled after the
existing `groundhog_db_gateway` crate. `DbGateway` is the right precedent
for naming, intro/version negotiation, generated-client shape, and
hogd-dev readiness advertising, but it is not the right service to
overload with contract execution: it is hogdb's latest-snapshot storage
read interface, and adding view execution there would give hogdb an
executor dependency. View execution should live behind a distinct
executor-owned or dedicated view service.

The standalone protocol-rs/xdrs migration landed on `groundhog-rpc`
`origin/main` in v0.3.3 / PR #73. View adapter work should be rebased on
that commit before adding `groundhog_view`; do not hide any remaining
dependency migration fallout inside the view wiring PR.

Recommended changes:

1. Add `view.x` to the `protos` generation list so
   `groundhog-xdr/src/view.rs` is generated.
2. Add the service XDR file to a new generation list, for example:

   ```bash
   views=(
     view_gateway.x
   )
   ```

3. Add a new workspace crate, recommended name `groundhog_view`, with
   dependencies matching the other generated RPC crates:

   - `groundhog-xdr`
   - `xdrs`
   - `xdr-common`
   - `rpc_driver`
   - `schemars`
   - optional `serde`, `serde_arrays`, `serde_with` if needed for schema
     or tests

4. Generate the service module into `groundhog_view/src/view_gateway.rs`.
5. Re-export the service module and the relevant XDR view types from
   `groundhog_view/src/lib.rs`.
6. Add `groundhog_view` to the workspace members and workspace
   dependencies.
7. Add it to protocol-rs workflow/CI builds.
8. Publish a protocol-rs release containing both the data types and the
   generated client.

The generated crate should expose a client type analogous to the current
generated clients, for example:

```rust
ViewGateway_ViewGateway1_Client
```

The exact generated name follows x23c output; `groundhog-rpc` only needs
a stable published crate and method.

## Hogd-Dev Readiness

When hogd-dev starts the view service, readiness should advertise it in
the existing `endpoints` object. Endpoint keys should match x23 service
names rather than `rpc-server` plane names:

```json
{
  "status": "ready",
  "endpoints": {
    "scheduler_coord": "127.0.0.1:9000",
    "indexer_block_explorer": "127.0.0.1:9001",
    "db_gateway": "127.0.0.1:9002",
    "view_gateway": "127.0.0.1:9003"
  }
}
```

The `view_gateway` endpoint should be optional. If hogd-dev omits it,
node-supervisor keeps returning `NotImplemented` for `groundhog_view`.

## Groundhog-RPC Consumption Plan

Once the protocol-rs release exists, `groundhog-rpc` can wire the real
backend in a focused PR:

1. Rebase onto `groundhog-rpc` v0.3.3 or later, then add
   `groundhog_view`.
2. Configure a bounded `RpcServerConfig::view_gas_limit` for gateway and
   node-supervisor view backends, then use that value as
   `TransactionViewQuery.invocation.gas_limit`. The scaffold `u64::MAX`
   default is only a placeholder while view returns `NotImplemented`.
3. Parse optional `endpoints.view_gateway` in node-supervisor readiness.
4. Add a node-supervisor adapter in `crates/node-supervisor/src/view_backend.rs`.
5. Add a gateway adapter/handler alongside the existing gateway handler
   modules, reusing `groundhog-x23-transport`.
6. Override `ReadPlane::view` for node-supervisor and gateway backends.
7. Map service payloads to public API types:

   | View service result | JSON-RPC result |
   |---|---|
   | `ViewGatewayOk(Return)` | `ViewResponse { output: Return, events, height }` |
   | `ViewGatewayOk(Abort)` | `ViewResponse { output: Abort, events, height }` |
   | `ViewGatewayOk(Fault)` | `ViewResponse { output: Fault, events, height }` |
   | `ViewGatewayAbandoned` | `BACKEND_UNAVAILABLE`, message says hogd exhausted its abandoned-snapshot retry budget and retry is safe |
   | `ViewGatewayInternalError` | `BACKEND_UNAVAILABLE` |
   | x23 timeout | `BACKEND_TIMEOUT` |
   | missing endpoint | `NotImplemented` JSON-RPC error |

8. Add conformance coverage for successful `Return`, `Abort`, and
   `Fault` view responses, plus absent-endpoint `NotImplemented`.
9. Add e2e smoke once hogd-dev advertises the endpoint.

## Test Requirements

Protocol/protocol-rs:

- `view.x` data types generate and compile in `groundhog-xdr`.
- The view service x23 program generates a client and dispatcher.
- `groundhog_view` builds under the same workflow that builds
  `groundhog_scheduler`, `groundhog_indexer`, and `groundhog_db_gateway`.
- XDR round-trip tests cover:
  - `ViewGatewayOk(Return)`
  - `ViewGatewayOk(Abort)`
  - `ViewGatewayOk(Fault)`
  - `ViewGatewayAbandoned`
  - `ViewGatewayInternalError`
- Schema/serde tests cover the generated types if the crate exposes those
  features for downstream consumers.

hogd:

- Service wrapper returns the snapshot height used for execution.
- A concurrent commit cannot make the service report a different height
  than the one executed.
- Raw `Abandoned` from `view_transaction` retries against a later
  committed snapshot up to the service retry budget.
- A successful retry returns the later snapshot height used for that
  attempt.
- Retry exhaustion returns `ViewGatewayAbandoned`.
- `caller = None` preserves root-frame behavior.
- `caller = Some(addr)` makes `get_msg_sender` observe `addr`.
- State writes and nested state writes fail under read-only execution.
- Executor `InvalidInput` is mapped according to this spec before
  crossing the service boundary.
- hogd-dev readiness includes `endpoints.view_gateway` only when the
  service is actually listening.

groundhog-rpc:

- Missing `endpoints.view_gateway` remains `NotImplemented`.
- Present `endpoints.view_gateway` maps `Return`, `Abort`, and `Fault` as
  JSON-RPC successes.
- `height` round-trips from the service payload.
- `ViewGatewayAbandoned`, internal errors, and timeouts use the existing
  view error mapping.
- Malformed service payloads are rejected instead of producing partial
  JSON.
- Gateway and node-supervisor produce byte-identical JSON for the same
  scripted `Return`, `Abort`, and `Fault` service results.

## Open Decisions Before Publish

- Final service file and crate name. Recommendation: `view_gateway.x` and
  `groundhog_view`.
- Final x23 program number. The implementation must reserve a unique
  value; `0x56475759` is a proposed mnemonic placeholder.
- Final hogd service retry budget for raw `Abandoned` attempts.
  Recommendation: keep `N` single-digit so sustained chain advancement
  cannot turn one public view request into a long backend stall.
- Whether hogd accepts the preferred optional `auth_claim` shape or needs
  a temporary ignored sentinel for compatibility.
- Whether `InvalidInput(error_code)` detail should remain service-local
  or force a public JSON-RPC `Fault` detail before wire stabilization.
- Whether v1 changes scope to support ephemeral view events. The default
  in this spec is strict read-only public view with an empty `events<>`
  list.
