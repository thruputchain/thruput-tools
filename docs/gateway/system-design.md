# RPC Gateway: System Design

**Status:** Architecture background updated for the current gateway shape. The
OpenRPC snapshot and `GroundhogApi` trait are the API source of truth.
**Context:** This records why the repo uses a separate Rust gateway service and
which backend responsibilities the gateway assumes.

Cross-team open questions and resolved decisions live in
[`open-questions.md`](open-questions.md) to keep this doc focused on
the settled architecture.

---

## Decision: Separate Gateway from Day One

We're building the JSON-RPC gateway as a standalone Rust service, not
embedding it in chuckd or chuckd-rs. Reasons:

1. **Every chain that scaled landed here.** Stellar (Horizon → RPC),
   Sui (RPC 2.0), Aptos (indexer + Hasura). Starting separated avoids
   a migration later.
2. **No dependency on chuckd-rs correctness.** The gateway talks to
   the validator stack over x23 RPC — same protocol hogd and chuckd
   already use. If chuckd-rs proves solid, we can offer an in-process
   deployment mode later by implementing the same handler traits
   against direct Rust calls. That's a future option, not a commitment.
3. **Independent scaling.** Read-heavy RPC traffic doesn't compete
   with consensus. Multiple gateway instances can front a single
   validator.
4. **Team parallelism.** Gateway work doesn't block or get blocked by
   coordinator work.

### Reference Architectures Studied

| Chain | Architecture | Key Takeaway |
|-------|-------------|--------------|
| **Sui** (`sui-indexer-alt-jsonrpc`) | Separate Rust binary. jsonrpsee + axum + Postgres. Three-tier data access (PG, Bigtable, gRPC consistent store). | Best reference for jsonrpsee patterns, module registration, middleware stack. Over-engineered data layer for our needs. |
| **Stellar RPC** | Separate Go binary. Embedded captive core + SQLite + jrpc2. 10 methods. | Simplest architecture. Good middleware patterns (per-method rate limiting, backlog queues). Closest to our Phase 1 scope. |
| **chuckd-rs** | 21-crate workspace. axum wallet stub, tokio throughout, x23 RPC to hogd. | Patterns worth copying (CancellationToken lifecycle, TxSubmitter trait). Not importing as a dependency. |

---

## Architecture

```
                   ┌───────────────────────────────────┐
                   │        hog-gateway service       │
  Developers ──────│  jsonrpsee (HTTP + WS)            │
  SDKs             │  ┌─────────┐  ┌────────────────┐  │
  Block Explorer   │  │ Handlers│  │ Subscriptions  │  │
  CLI              │  └────┬────┘  └───────┬────────┘  │
                   │       │               │           │
                   └───────┼───────────────┼───────────┘
                           │               │
              ┌────────────┼───────────────┼──────────┐
              │            │               │          │
              ▼            ▼               ▼          │
     ┌─────────────┐ ┌───────────┐ ┌────────────┐     │
     │ hogd        │ │ hogd      │ │ chuckd     │     │
     │ indexer     │ │ scheduler │ │ coordinator│     │
     │ (RocksDB)   │ │           │ │            │     │
     └─────────────┘ └───────────┘ └────────────┘     │
      x23 RPC        x23 RPC       x23 RPC            │
                                                      │
      receipts,       tip, submit,   block commit     │
      contract        mempool,       notifications    │
      events          tx lookup      (Phase 2)        │
```

### JSON-RPC Arrays

The gateway and embedded dev node dispatch every nonempty JSON-RPC array with
bounded concurrency, including mixed methods. The window defaults to 128 entries,
counting both active work and completed responses awaiting earlier entries.
Execution order is unspecified; responses retain request order. An array is
not a transaction or a sequence: a read can run before an earlier mutation
finishes. Send dependent requests separately and wait for the required state.

Notifications use jsonrpsee's notification path, not its call path, and add no
response. Empty arrays retain jsonrpsee's invalid-request response. Completed
responses share the response byte limit even when an earlier entry is pending;
overflow cancels outstanding dispatch immediately. Cancellation does not roll
back side effects already accepted by a backend.

Override the window with `hog-gateway --rpc-batch-concurrency 256` or
`groundhog node --rpc-batch-concurrency 256`. The gateway also accepts the
top-level TOML key `rpc_batch_concurrency = 256`; the CLI overrides TOML.
Supervisor embedders use `NodeSupervisorConfig::with_rpc_batch_concurrency`
with a validated `RpcBatchConcurrency` value. Values outside 1..=1024 fail
configuration parsing; 1 gives sequential array dispatch.

The 1024 ceiling bounds per-array work even on the embedded server, which has
no array-entry count cap. The gateway's existing 1000-entry array cap remains
unchanged. This is not a global concurrency cap and does not change response
byte limits, scheduler submit batch size, in-flight backend batch limits, or
storage batching. Those backend controls remain independent.

Airdrop handlers also separate execution slots from bounded waiting capacity.
Local airdrops allow 16 outstanding requests per configured execution slot
(256 outstanding and 16 executing by default); the hosted gateway adapter allows
1024 outstanding and 64 executing. Queueing and execution share one request
deadline. Full admission, attempt-store capacity, and upstream faucet quotas can
still produce per-entry overload errors.

### Data Sources (via x23 RPC)

| Data | Source | Existing RPC | Notes |
|------|--------|-------------|-------|
| Transaction receipt (events, height, status, gas used) | hogd indexer | `get_transaction_info(tx_hash)` | Returns `TxInfo { height, tx, TxEventLog }`; `TxEventLog.gas_consumed` is consensus-committed |
| Contract events (paginated, filterable) | hogd indexer | `get_events(EventQuery)` | Returns `EventPage` with cursor + page metadata |
| Chain tip (height, hash) | hogd `SchedGateway` | `schedgateway_get_tip()` | |
| Transaction submission | hogd `SchedGateway` | `schedgateway_submit_tx_batch(batch)` | Returns one request-scoped admission outcome per exact signed transaction |
| Transaction lookup (pending) | hogd `SchedGateway` | `schedgateway_lookup_tx(hashes)` | Returns `Option<SignedTransaction>` per hash |
| Pending tx count | hogd `SchedGateway` | `schedgateway_pending_tx_count()` | Atomic counter, includes runqueue + stranded |
| Latest committed storage | hogd DbGateway | `get_storage_at(address, key)` | Optional endpoint; required for `groundhog_getStorageAt` |
| Sandboxed view execution | hogd ViewGateway | `view(TransactionViewQuery)` | Optional endpoint; required for `groundhog_view` |
| Block/CV data by height or hash | chuckd coordinator | **TBD** — see open questions | `DiskStorage` exists but not exposed for queries |

### What Needs x23 Exposure

Most query paths already exist. Gaps:

- **Block data by height/hash**: `DiskStorage.lookup_cv_by_height()`
  and `lookup_catchup_block()` exist in chuckd-rs but are only
  accessible through the catchup/repair peer protocols, which use
  signed requests. A lightweight query endpoint is needed, OR hogd
  needs to index block headers. See `open-questions.md`.

- **Block commit notifications**: `Notify` interface exists (chuckd)
  but is stubbed (`IgnoreNotify`). Phase 2 needs this wired to a
  broadcast channel that the gateway can subscribe to. See `open-questions.md`.

---

## Transaction Submission

`groundhog_sendRawTransaction` forwards to hogd's public `SchedGateway`
admission method; read paths use `SchedGateway` and the hogd indexer directly.
The internal `SchedulerCoord` service remains reserved for coordinator
operations such as genesis and block driving. This keeps public ingress close
to the authoritative mempool admission point without exposing the coordinator
protocol to gateway clients.

As the component that first accepts internet-originated transactions,
the gateway owns rate limiting, dedupe, backpressure, overload shedding,
and any future QoS policy.

### Client-facing submit semantics

`groundhog_sendRawTransaction` preserves this external contract:

- success means the transaction was admitted by the authoritative downstream
  mempool path; it does not imply nomination or inclusion in a committed block
- `TxRejected` means the exact signed transaction was definitely not admitted
- retryable and internal admission outcomes map to overload and backend errors
- an x23 RPC failure is indeterminate admission, not a rejection
- transactions that can never commit because `blockExpiryHeight` is at
  or below the current chain tip are rejected with `TxRejected` before
  submission. The pre-check is best-effort: if the tip is unavailable
  the transaction is submitted anyway and the scheduler enforces expiry
  authoritatively.

### V1 posture

For testnet v1, the goal is not to solve final-form proposer-ingress economics.
The goal is to establish the correct boundary:

- dedicated public RPC gateway
- private-network hop to validator components
- one explicit admission layer in the gateway
- a submission path that preserves mempool-admission semantics

The v1 policy can stay intentionally simple:

- deterministic dedupe
- basic per-IP / per-key rate limits
- bounded queues
- overload shedding
- metrics

That is enough to be architecturally sound for testnet while leaving room for
later QoS or quota systems.

---

## Threat Model: Proposer-Local Ingress Races

The gateway is not just a convenience layer. It is the primary public
defense boundary for Groundhog's local-producer race.

Groundhog validators do **not** each rerun a concurrent proposal race and
independently decide which conflicting `NNInt` reservation wins. The
proposer runs proposal scheduling, packages the winning tx set plus any
required nondeterministic metadata, and validators replay that exact
proposal during validation.

That means:

- the system is not exposed to consensus divergence from every validator
  making a different local race decision
- the remaining game is concentrated at proposer-local ingress and
  proposal execution
- the public gateway layer is therefore the natural place for anti-spam
  and fairness controls

The practical risk is not "all nodes fork because threads raced
differently." The practical risk is that searchers or abusive clients try
to win contested local reservations by dominating the proposer's public
ingress path.

This is why gateway design should be framed as an admission-control and
defense problem, not only as a throughput or topology problem.

See also [Transaction Ingress and Proposer-Local Races](tx-ingress-and-proposer-race.md)
for the code-verified version of this threat model, and
[Roadmap](roadmap.md) for the staged plan from thin testnet gateway to more
advanced ingress policy.

---

## Wire Format Conventions

All JSON-RPC responses follow these serialization rules:

| Type | JSON Format | Example |
|------|------------|---------|
| Hashes (`SHA256Hash`, `TxHash`) | `0x`-prefixed lowercase hex, 64 chars | `"0xa1b2c3d4..."` |
| Addresses (`ContractAddress`) | `0x`-prefixed lowercase hex, 64 chars | `"0x00112233..."` |
| Event topics (`Topic`) | `0x`-prefixed lowercase hex, 64 chars | `"0xdeadbeef..."` |
| Event data (`HexBytes`) | `0x`-prefixed lowercase hex, variable length | `"0xcafe"` |
| Block height, tx count, etc. | JSON number (decimal integer) | `42` |
| Enums (`TransactionStatus`, `ExecStatus`) | Tagged JSON object | `{"status":"committed","height":42,...}` |

**Rationale:** Ethereum's `0x`-hex for bytes is the de facto standard.
Ethereum's hex-encoded *quantities* (`"0x1a4"` for block 420) are widely
considered a UX mistake — Solana, Sui, Aptos, and Stellar all use plain
integers. We take the best of both: hex bytes, decimal numbers.

**Note on `groundhog-xdr`:** The XDR crate serializes hash/address types as
bare hex (no `0x` prefix) via `serde_arrays::hex`. The public RPC boundary wraps
byte-like values in `json-rpc-types` so JSON-RPC clients see the
`0x`-prefixed format consistently.

### Structured error data

JSON-RPC error objects carry `code` and `message` for every catalog error.
`data` is present only when the server has a typed detail to offer, and its
shape is a tagged object modeled in `json-rpc-types`:

| Error | `data` type | Discriminator |
|-------|-------------|---------------|
| `TxInvalid` (`-32000`) | `TxInvalidDetail` | `reason`: `tooManyContracts`, `contractTooLarge`, `duplicateContract`, `rv32MemoryTooLarge`, `rv32CodeTooLarge` |

Example rejection from the `sendRawTransaction` pre-checks:

```json
{
  "code": -32000,
  "message": "invalid transaction: transaction attaches 5 contracts; chain permits 4",
  "data": { "reason": "tooManyContracts", "attached": 5, "max": 4 }
}
```

The pre-checks mirror the executor's deterministic pre-execution gate
(`SystemCallHandler::perform_initial_validation` in hogd): attached-contract
count against the `maxNewContractsPerTx` parameter of the tip's network
config, each RV32 blob against its `maxContractSizeBytes` parameter, no
contract attached twice, and each RV32 artifact header's declared guest
memory and code size against `maxRv32MemoryBytes` and `maxRv32CodeBytes`
(what the executor's attachment verification enforces; memory is checked
before code, as hogd's artifact parser orders them). A blob without a
readable v5 header is left to the executor. The bound logic is the
`groundhog-tx-validation` crate. The
network config arrives with every `schedgateway_get_tip` reply (the same
one that serves `chainTip`); the shared server flattens it into
`NetworkConfigParams` once per fetch and keeps a single cached db-tip
reading (height plus params) that every fetch refreshes: a submit that
finds it older than `tip_cache_ttl_ms`, the public `chainTip` method, and
a background task that probes every `tip_refresh_interval_ms` on an idle
server. The public `chainConfig` method reads through that same cache, so
however often it is called it costs the backend at most one fetch per
`tip_cache_ttl_ms`; on a cold cache a caller waits for the in-flight probe
rather than answering empty. It reports exactly the stated params and fails
rather than answering when there is no reading or the config version is
unreadable. For the pre-checks, a
parameter the config does not state is `None` and its check is
skipped, except where the protocol itself fixes the value in force for an
unstated config: `maxContractSizeBytes` falls back to
`kDefaultMaxContractSizeBytes` (256 KiB), the cap every chain predating the
parameter ran under, and `maxRv32MemoryBytes` and `maxRv32CodeBytes` fall
back to `kDefaultMaxRv32MemoryBytes` and `kDefaultMaxRv32CodeBytes` (64 KiB
each, from the protocol's `hog_exec_constants.x`). hogd's own fallbacks are
never copied. A bound
violation found against a cached reading is re-checked against a freshly
fetched tip before it is returned, so a reading that lags a config-upgrade
block never rejects what the current config admits. A stale reading
can otherwise only lag a config-upgrade block, and a missing one skips the chain-derived checks
so an unreachable scheduler never blocks submission on its own. The
executor re-checks everything, so these are a round-trip saver, not a
trust boundary. The OpenRPC artifact documents each payload shape on its
error component as `x-data-schema`.

---

## Event Parsing

Hogd stores events as `TxEmittedEvent`:

```
TxEmittedEvent {
    contract_address: ContractAddress,
    num_topics: u32,           // 0, 1, 2, or 3
    serialized_event: Vec<u8>, // max 1024 bytes total
}
```

`serialized_event` concatenates topics and data:

```
┌──────────┬──────────┬──────────┬──────────────────┐
│ topic 0  │ topic 1  │ topic 2  │ data (remainder) │
│ 32 bytes │ 32 bytes │ 32 bytes │ variable         │
└──────────┴──────────┴──────────┴──────────────────┘
          ↑ only if num_topics >= 2
                     ↑ only if num_topics == 3
```

The gateway must split this into the API's `EmittedEvent`:
```rust
EmittedEvent {
    contract_address: ContractAddress,
    topics: Vec<Topic>,   // num_topics × 32-byte Topics
    data: HexBytes,       // remaining bytes after topics
}
```

Validation: `serialized_event.len() >= num_topics * 32`.

---

## Transaction Status Lookup Order

Scheduler lookups share a gateway-local bound of 68 outstanding calls. By default,
16 execute on a dedicated connection pool, leaving room for 52 asynchronous
waiters when all execution slots are occupied. `scheduler_lookup_pool_size`
(`--scheduler-lookup-pool-size`) configures execution width from 1 to 68 without
changing total admission. Queueing,
execution, and indexer fallback share one absolute backend deadline. A lookup
deadline or cancellation releases permits and leaves its connection in
service, without invalidating the scheduler's normal submit/tip lane. Actual
RPC failures invalidate only the matching scheduler generation. This finite
backend bound is independent of the default JSON-RPC array concurrency of 128:
a sufficiently contended array can intentionally receive per-entry `Overloaded`
errors. Terminal indexer answers may still resolve admission failures, but an
indexer miss must not turn a failed scheduler admission into `Unknown`.

Lookup admission and local waiting are exposed independently of backend calls:

- `groundhog_gateway_scheduler_lookup_admission_total{outcome}` counts
  `accepted`, `full`, and `closed` decisions, even when indexer fallback makes
  the public RPC succeed. The existing backend `outcome="rejected"` series is
  retained.
- `groundhog_gateway_scheduler_lookup_in_flight{stage}` reports `queued`
  requests waiting for an execution permit and `executing` requests holding a
  permit. Executing includes waiting for the scheduler client read lock and
  backend work; cancellation releases the gauge with the worker's permits.
- `groundhog_gateway_scheduler_lookup_wait_duration_seconds{stage,outcome}`
  is a histogram for `execution_permit` and `scheduler_client` waits. Outcomes
  are `ready`, `timeout`, `closed`, or `cancelled`. Ready means a permit or the
  context read lock was acquired, not that the scheduler is healthy or that a
  transport connection was acquired. Closed applies to the execution semaphore.

Compare the wait histograms with backend duration for
`operation="schedgateway_lookup_tx"`. Backend timing starts after the client
read lock and includes worker scheduling, x23 transport/pool waiting, and call
completion or timeout cleanup. It does not isolate scheduler processing time.
High permit waits with short backend calls point toward local burst/queue
pressure; long backend calls need transport and scheduler investigation.
Admission counters preserve short saturation bursts that scrape-time gauges
can miss. Wait histograms include cancellation and timeout, so completed calls
do not hide abandoned queue waits. No transaction hash is used as a metric label.

`getTransactionStatus` checks the indexer first. A committed record returns
immediately without occupying the scheduler lookup lane. Otherwise it checks
the scheduler: a mempool hit returns `Pending`, including when the indexer is
unavailable or holds an advisory failure from an earlier attempt. Without a
mempool hit, a recent-failure diagnostic is returned. Only successful misses
from both backends produce `Unknown`. A transaction that commits between the
two reads is reported `Unknown` once; the client's next call sees it.
That sequence is `groundhog_hogd_backend::status::transaction_status`, shared
with `groundhog node`: each binary supplies its two raw lookups through the
`StatusProbe` trait, so the gateway keeps its admission lane, deadline, and
metrics inside those lookups and the two binaries cannot order the probe
differently.

Committed reads use one backend RPC; pending and unknown reads use two. Both
stages share one deadline, so a slow indexer can consume the budget before a
pending lookup is dispatched.

`RecentFailure` is best-effort and advisory: the indexer caches
proposal-time two-phase-commit failure diagnostics for a few blocks,
reports are dropped freely under load, only the proposing node's
indexers hear them, and a later retry of the same transaction may
still commit (at which point `Committed` wins). Absence of the status
proves nothing. Receipts are unaffected: a failed transaction is
excluded from the block, so `getTransactionReceipt` returns `null`.

**Degraded-mode behavior:**

| Scheduler | Indexer | Behavior |
|-----------|---------|----------|
| Up | Up | Full status (Pending / Committed / RecentFailure / Unknown) |
| Down | Up | Terminal indexer answers (Committed / RecentFailure) still return. On an indexer miss, return `BackendUnavailable` instead of Unknown: without a mempool answer the gateway cannot claim the transaction is nowhere, and a still-pending transaction must not read as gone. |
| Up | Down | A scheduler hit can still return Pending if budget remains; otherwise return an error. An indexer failure cannot produce Unknown. |
| Down | Down | Return `BackendUnavailable` error. |

`Unknown` is therefore a positive claim — "not in the mempool and not
in the indexer" — only produced when both backends answered.

A scheduler timeout, unavailability, or overload never turns an indexer miss
into `Unknown`.


### x23 data pools

Every x23 data pool is an `rpc_driver::MultiplexPool` built through
`x23-transport`: many calls in flight per connection, each routed to the
connection with the fewest outstanding. A pool's size setting
(`indexer_pool_size`, `scheduler_lookup_pool_size`, and the batcher-derived
scheduler and `DbGateway` widths) is its execution width, enforced in the
transport as a cap on calls in flight, and every data pool opens
`x23_multiplex_connections` (`--x23-multiplex-connections`, default 4)
sockets. Roughly one connection per worker the backend serves with is the
useful range; each connection's read loop is one task, so more sockets do not
add throughput.

Admission is unchanged: lookups stay under the 68-call bound with
`scheduler_lookup_pool_size` execution permits, and the batchers keep their
in-flight caps. Health probes use one serial `FixedSerialPool` connection,
which a canceled probe poisons. A canceled data call's late reply is discarded
and its connection stays in service; the gateway does not depend on a
connection-level reaction to a canceled call. A submit timeout invalidates the
scheduler generation, which drops all of that generation's pools, and the
health loop rebuilds them behind the readiness grace. A lookup timeout aborts
only that lookup's worker and leaves the generation alone.

---

## Event Query Bounds

`get_events` is paginated server-side (the indexer clamps page size to
its own cap), but the gateway adds a defensive pre-XDR check before
encoding so clearly-bogus inputs fail at the JSON-RPC boundary:

- **Per-page limit:** Configurable, default 200.  Requests with
  `limit` above this return `INVALID_PARAMS`.
- **Cursor length:** Configurable, default 256 bytes.  Longer opaque
  cursors return `INVALID_PARAMS`.
- **`limit == 0`** passes through unchanged — the indexer treats it
  as "use server default".

---

## Crate Structure

```
groundhog-rpc/                          workspace
├── .cargo/config.toml                  private registry config
├── Cargo.toml                          workspace root
├── crates/
│   └── json-rpc/
│       ├── json-rpc-types/         shared RPC types
│       ├── json-rpc-api/               jsonrpsee trait definition
│       ├── json-rpc-server/            shared server implementation
│       ├── json-rpc-gateway/           standalone JSON-RPC server binary
│       ├── json-rpc-client/            typed JSON-RPC client library
│       ├── open-rpc/                   OpenRPC document generator
│       ├── open-rpc-macros/            OpenRPC proc macro
│       └── open-rpc-spec/              canonical generated spec artifact
```

### Dependency Flow

```
groundhog-xdr  (private registry, from protocol-rs)
xdrs           (private registry)
rpc_driver     (private registry, x23 RPC framework)
       │
       ▼
json-rpc-types   (this workspace — shared by all consumers)
       │
       ▼
json-rpc-api         (method trait)
       │
   ┌───┴─────────┐
   ▼             ▼
json-rpc-server json-rpc-client
   │             │
   ▼             └──→ SDK, CLI tool, block explorer
json-rpc-gateway
   │
   └──→ deployed as standalone service
```

### Type Unification

`json-rpc-types` is the shared layer between `groundhog-xdr` (wire
format) and what developers see. It re-exports XDR types where appropriate and
defines RPC-specific types (receipts, status enums, error codes).

**Consumed by:**
- `json-rpc-gateway` and `json-rpc-server` (server-side serialization)
- `json-rpc-client` (client-side deserialization)
- SDKs, CLI tools, and block explorers via the typed client

**Not in `json-rpc-types`:** contract-execution types (those stay in
`groundhog-xdr` and `sdk`), consensus internals, SCP types.

### Client Crate Trait Location

Resolved: the `#[rpc(client, server)]` trait lives in
`crates/json-rpc/json-rpc-api/src/lib.rs`. It generates both
`GroundhogApiServer` for the shared server implementation and
`GroundhogApiClient` for `json-rpc-client`, keeping one source of truth for the
public JSON-RPC method surface.

### Private Registry Dependencies

```toml
# .cargo/config.toml
[registries.groundhog]
index = "..."  # private registry URL

# Cargo.toml (workspace root)
[workspace.dependencies]
groundhog-xdr = { version = "0.3.5", registry = "groundhog" }
xdrs = { version = "0.5", registry = "groundhog" }
rpc_driver = { version = "0.3", registry = "groundhog" }
jsonrpsee = { version = "0.24", features = ["server", "client", "macros"] }
axum = "0.8"
tokio = { version = "1", features = ["full"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = "0.3"
prometheus = "0.13"
```

---

## Phasing

### Phase 1: Submit + Query (Target: private testnet)

The current public surface is generated from `crates/json-rpc/json-rpc-api`.
Reads go through x23-backed gateway adapters where the corresponding hogd
endpoint exists. HTTP transport is the current focus; WebSocket subscriptions
remain a later phase.

| Method | Data Source | Notes |
|--------|-----------|-------|
| `groundhog_sendRawTransaction(bytes) → TxHash` | hogd `SchedGateway` | Return the hash only after `Admitted`; this does not imply nomination or commitment |
| `groundhog_getTransactionReceipt(hash) → Receipt` | indexer `get_transaction_info` | No `storage_delta` in Phase 1; `gasUsed` from `TxEventLog.gas_consumed` |
| `groundhog_getTransactionStatus(hash) → Status` | `SchedGateway.schedgateway_lookup_tx` + indexer | See lookup order above |
| `groundhog_chainTip() → TipInfo` | `SchedGateway.schedgateway_get_tip` | |
| `groundhog_chainConfig() → NetworkConfigParams` | `SchedGateway.schedgateway_get_tip` | Stated execution parameters only; an unstated parameter is absent, never defaulted |
| `groundhog_getBlockByNumber(height) → BlockInfo` | **TBD** | In OpenRPC; handler is not implemented until a block query backend exists |
| `groundhog_getEvents(EventQuery) → EventPage` | indexer `get_events` | Gateway enforces per-page limit and cursor length |
| `groundhog_pendingTransactionCount() → u64` | `SchedGateway.schedgateway_pending_tx_count` | In OpenRPC; gateway handler is not wired yet |
| `groundhog_getStorageAt(address, key) → StorageItemRead` | DbGateway | Requires `db_gateway` endpoint |
| `groundhog_view(request) → ViewResponse` | ViewGateway | Requires `view_gateway` endpoint |
| `groundhog_version() → VersionInfo` | gateway build metadata | Reports wire format and component identity; includes release version and bundle-manifest digest only after a validated manifest match |

Plus `rpc.discover` for free from jsonrpsee's OpenRPC generation.

At startup, `hog-gateway` logs its component version and source commit. When it
finds a valid installed `bundle-manifest.json` whose `groundhog-rpc` component
matches that build, the log and `groundhog_version` also include the containing
release version and manifest digest. Loose binaries keep reporting their
component identity without claiming a release.

Admission policy for this phase should stay intentionally minimal: basic rate
limits, dedupe, bounded queues, and overload shedding. Mainnet-grade QoS and
economic spam controls are later-phase concerns.

### Phase 2: Subscriptions (WebSocket)

- `groundhog_subscribeNewBlocks` — committed block stream
- `groundhog_submitAndWatch(bytes)` — tx lifecycle stream
- Requires wiring `Notify` interface to broadcast channel

### Phase 3: SDK Client + CLI

- `json-rpc-client` crate auto-generated from `#[rpc(client)]` trait
- SDK wraps it as `GroundhogClient`
- CLI uses it for `groundhog tx submit`, `groundhog tx status`, etc.

### Phase 4: Production Hardening

- Rate limiting (per-IP token bucket)
- Request size limits
- Per-method Prometheus histograms
- Cursor-based pagination on event/block range queries
- DenyUnsafe for public nodes
- API key auth (optional)
