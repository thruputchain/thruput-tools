# Gateway design

The gateway provides a public JSON-RPC boundary in front of hogd's internal
x23 services. Keeping it separate lets operators scale and protect public
traffic independently of validator execution. It also keeps authentication,
quotas, queueing, and deployment policy out of transaction-validity rules.

## Shared server, separate lifecycles

[json-rpc-server](../../crates/json-rpc/json-rpc-server/src/server.rs) implements
the public methods against backend traits. The production gateway and local
node supervisor supply adapters. This keeps validation and public errors
consistent while allowing different connection, recovery, and process policies.

[hogd-backend](../../crates/x23/hogd-backend/src/lib.rs) owns XDR result decoding
and shared transaction-status ordering. It has no connection ownership or
retry policy. Gateway adapters own deadlines, admission, metrics, and reconnects;
the supervisor also owns a child process. New response mappings belong in the
shared crate so the same backend result has one meaning in both deployments.

Backend responsibilities are distinct:

- `SchedGateway` admits transactions and serves scheduler reads.
- The indexer serves committed receipts, events, and transaction history.
- `DbGateway` serves committed state reads.
- `ViewGateway` executes sandboxed contract views.
- `SchedulerCoord` is for coordinator control, including local block driving;
  it is not a substitute public-submit endpoint.

The [runtime manifest](runtime-manifest.md) connects local tools to those
services. Production topology comes from gateway configuration.

## Shared-snapshot reads

Single views and individual batch entries accept optional `authClaim` (wire
11.2). A claim is assumed without signatures; normal contract checks still run
and writes never commit. Omission supplies the existing zero-key, zero-weight
source-address claim. `caller` separately identifies an optional parent contract.

Claims use `kind: "sourceAddress"` with uint32 `allowedKeys` / `requiredWeight`,
or `kind: "pubkeySignature"` with an Ed25519 `publicKey` (base58 encoding of 32 bytes) and a
decimal-string uint64 `nonce`. No private key or signature is accepted.

Single `groundhog_view` calls also accept `minHeight`. The shared RPC server
checks the height returned by the existing single-view execution path and rejects
an older result with `ViewHeightUnavailable`. Omitting the option preserves the
normal single-view behavior. Single views retain their own gas, response, and
deadline policy; they do not inherit aggregate batch limits.

`groundhog_viewBatch` runs independent calls against one committed snapshot.
Calls retain input order and do not share simulated writes. A failed snapshot
attempt discards all partial results; hogd retries the group at most three times.

`minHeight` is a freshness floor. If the selected snapshot is older, the request
returns `ViewHeightUnavailable` with requested and observed heights. It does not
wait for a block or request a snapshot refresh. Hogd maintains snapshots through
its shared refresh worker; callers choose whether and when to retry.
For a batch, the floor belongs to the group. A `minHeight` field inside a call
entry is rejected rather than ignored.

Batch limits are operator policy, not consensus parameters. The gateway's TOML
defaults are:

```toml
view_timeout_ms = 30000

[view_batch]
max_calls = 16
max_calldata_bytes = 65536
max_gas = 160000000
max_response_bytes = 1048576
```

The shared server validates call count, aggregate calldata, and declared gas.
Per-call view limits also apply. The aggregate gas cap is checked in RPC before
dispatch; hogd enforces each call's gas limit. Hogd also receives the remaining
time and encoded response-size budget, and checks them during execution. The
response-size budget includes events and measures XDR bytes; the public JSON
response and transport message caps still apply. Timeout settings bound the
public response wait, not cancellation of running VM work or database I/O.

`serverLimits.viewBatch` advertises the endpoint policy. The embedded supervisor
uses the same `ViewBatchConfig` and its existing `view_timeout` setting. Limits
can change between requests; clients must handle rejections and must not split a
group into separate snapshots automatically.

## Admission and transaction outcomes

Successful submission means the authoritative mempool admitted the transaction.
It does not mean the transaction was nominated, executed successfully, or
committed. A transport failure can leave admission unknown; it must not be
reported as definite rejection or silently retried as a new transaction.

The shared server's pre-checks improve error reporting for transactions the
executor would reject. They are advisory: the executor remains authoritative.
They use the chain's network configuration and protocol-defined defaults,
not a second set of locally invented limits. See
[transaction validation](../../crates/tx-validation/src/lib.rs) and the
[RPC error catalog](../../crates/json-rpc/json-rpc-types/src/error.rs).

The gateway is the public admission-policy boundary. Transaction validity and
fairness are different questions: validating a proposed block does not establish
that public clients received fair access to proposal capacity. Rate limits,
quotas, and overload shedding belong at this boundary, with deployment-specific
policy. A bounded connection pool alone does not establish a complete denial-of-service
or fairness policy. The existence of this boundary is not evidence that every
possible protection is implemented; inspect gateway configuration and admission
code for the controls a deployment actually has.

### x23 data pools

[x23-transport](../../crates/x23/x23-transport/src/lib.rs) supplies shared pool
construction. Multiplexed connections allow several calls in flight per socket,
so socket count is not a backend-work limit. Admission limits, execution permits,
response bounds, and deadlines protect different resources. Cancellation frees
local waiting capacity but cannot roll back work already accepted downstream.

Use [GatewayConfig](../../crates/json-rpc/json-rpc-gateway/src/config.rs) and
`hog-gateway --dump-effective-config` for the actual controls and defaults.
The [lookup benchmark](lookup-bench.md) distinguishes successful work, overload,
local waiting, and backend duration.

## Transaction Status Lookup Order

Indexed commitment wins immediately. Otherwise the shared probe looks up pending
and retained scheduler outcomes, returning an advisory indexer failure diagnostic
only after a successful scheduler miss. Scheduler retry and dropped outcomes
preserve their error source and protocol error code.
Scheduler success reports `Committed` with its committed block height, even before
the indexer has a record. Indexed records also supply the actual height, including
block zero. Every committed status includes its height; receipts can lag behind.
`Unknown` requires successful misses from both backends. Backend unavailability
cannot establish absence. These rules prevent an unavailable scheduler from
making a pending transaction appear to have disappeared.

Reads across services are not an atomic snapshot. A transaction can commit
between reads, so a later poll may change an `Unknown` answer. A recent failure
is advisory and does not prove a transaction can never commit; a receipt records
a committed execution result.

The exact sequencing and degraded-mode behavior live in
[status.rs](../../crates/x23/hogd-backend/src/status.rs) and its tests. Gateway
admission and timing remain in its adapter; both binaries share the sequencing.

## Batching does not change transaction boundaries

A JSON-RPC array groups requests for transport. Its entries can execute
concurrently; it is not an atomic transaction or a dependency-ordered program.
Send dependent operations separately and wait for the state they require.

Backend submit batching is a different optimization: it combines independent
submissions into fewer scheduler calls while preserving each signed transaction's
admission outcome. Public array concurrency, backend batch size, admission limits,
and socket count are separate controls. Timeout or cancellation cannot undo a
transaction already admitted by the scheduler. See the gateway
[submitter](../../crates/json-rpc/json-rpc-gateway/src/submitter.rs) and local
[submit batcher](../../crates/node-supervisor/src/submit_batcher.rs).

## Views and reads

A view is a sandboxed execution against a discarded scratchpad. It can report
simulated effects without committing them. That makes it useful for wallet
previews as well as read-style contract calls. It is not a historical replay
service or proof that a future submitted transaction will see the same state.

Public declarations do not guarantee a backend exists in every deployment.
For example, the standalone gateway's block lookup and pending-count adapters
currently return `NotImplemented`; see
[blocks.rs](../../crates/json-rpc/json-rpc-gateway/src/handlers/blocks.rs).
Optional backend availability and registered extensions must be checked for the
target being used.

## Wire Format Conventions

The public API projects binary XDR into JSON-friendly types: `camelCase` fields,
tagged unions, decimal quantities, and semantically typed byte strings.
Addresses, public keys, validator IDs, hashes and standalone signatures use
unprefixed, case-sensitive Bitcoin/Solana-alphabet base58. Legacy hex identifiers
are rejected. Serialized transactions, code, calldata, return/abort data,
storage bytes, raw event topics, storage keys, selectors and cursors retain
`0x`-prefixed lowercase hex. A raw topic stays hex even when its schema-decoded
value is an address or hash. Choose encoding by declared meaning, never length.
All 64-bit-or-wider integers use canonical decimal strings, even for small
values. Smaller bounded integers remain JSON numbers. Requests reject numeric
JSON for wide fields, leading zeros, `+` prefixes, negative zero, whitespace,
exponents, and overflow. This preserves nanosecond timestamps and full protocol
integer ranges through JavaScript's JSON parser. The shared
[decimal adapters](../../crates/json-rpc/json-rpc-types/src/decimal.rs) keep Rust
integers typed and generate the corresponding schemas; XDR is unchanged.
Opaque cursors are returned verbatim. The projection belongs in
[json-rpc-types](../../crates/json-rpc/json-rpc-types/src/lib.rs), rather than
separate gateway and client copies.

[json-rpc-api](../../crates/json-rpc/json-rpc-api/src/lib.rs) owns method signatures
and the wire-format version. The [OpenRPC snapshot](../../crates/json-rpc/open-rpc-spec/snapshots/thruput-spec.json)
is generated from those definitions. Its version is independent of the workspace
release; implementation and release identity fields are diagnostic.

## Event Parsing

The binary event format places fixed-size topics before event data. The public
projection separates them into topics and data after validating the boundary.
The decoder belongs in
[hogd-backend/event.rs](../../crates/x23/hogd-backend/src/event.rs); payload types
and their documentation live in
[json-rpc-types/events.rs](../../crates/json-rpc/json-rpc-types/src/events.rs).
Malformed backend events must not become misleading valid public events.

## Operating the gateway

Use the [runtime-manifest quickstart](runtime-manifest.md) for local wiring,
[observability guide](observability-smoke-test.md) for health and metrics, and
[benchmark guide](bench-scenarios.md) for repeatable measurements. Health proves
specific configured probes passed; it does not prove all public methods work or
that a load envelope is safe.
