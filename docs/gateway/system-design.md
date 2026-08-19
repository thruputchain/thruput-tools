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

### Data Sources (via x23 RPC)

| Data | Source | Existing RPC | Notes |
|------|--------|-------------|-------|
| Transaction receipt (events, height, status) | hogd indexer | `get_transaction_info(tx_hash)` | Returns `TxInfo { height, TxEventLog }` |
| Contract events (paginated, filterable) | hogd indexer | `get_events(EventQuery)` | Returns `EventPage` with cursor + page metadata |
| Chain tip (height, hash) | hogd scheduler | `get_tip()` | |
| Transaction submission | configurable: hogd scheduler or chuckd coordinator | `submit_unvetted(batch)` | Gateway config selects the private-network ingress path |
| Transaction lookup (pending) | hogd scheduler | `lookup_transactions(hashes)` | Returns `Option<SignedTransaction>` per hash |
| Pending tx count | hogd scheduler | `size()` | Atomic counter, includes runqueue + stranded |
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

- **A concrete Rust surface for a chuckd submit backend**: `protocol-rs`
  now provides generated Rust/XDR support for the direct scheduler path
  (`groundhog-xdr`, `groundhog_scheduler`), but it does not yet provide an
  equivalent generated x23 "submit unvetted to chuckd" surface. If we keep a
  configurable `chuckd_coordinator` submission backend, we still need to define
  its concrete transport contract.

---

## Transaction Submission Backend

The gateway should support two private-network submission targets:

- **`hogd_scheduler`**: `groundhog_sendRawTransaction` forwards directly to
  hogd's `submit_unvetted`.
- **`chuckd_coordinator`**: `groundhog_sendRawTransaction` forwards to
  chuckd, which relays the transaction to hogd.

This should be a deployment-time config choice, not a baked-in protocol
decision. The gateway still uses hogd scheduler/indexer directly for the
read paths; only tx ingress is switchable.

### Why both exist

- **Direct to hogd** keeps the public ingress as close as possible to the
  authoritative mempool admission point, removes one hop/queue, and
  now has a concrete generated Rust client path via `protocol-rs`.
- **Via chuckd** can simplify private-network topology and ACLs because
  hogd only needs to trust a single internal caller.

### What does not change

Routing through chuckd does **not** materially change the proposer-local
fairness posture by itself. If chuckd is only a relay, the pressure point
simply moves from `gateway -> hogd` to `gateway -> chuckd -> hogd`.
Whichever component first accepts internet-originated transactions still
owns rate limiting, dedupe, backpressure, overload shedding, and any
future QoS policy.

### Client-facing submit semantics

`groundhog_sendRawTransaction` should preserve the same external
contract in both modes:

- success means the transaction was accepted by the authoritative
  downstream mempool path
- failure means the transaction was rejected by that path
- transactions that can never commit because `blockExpiryHeight` is at
  or below the current chain tip are rejected with `TxRejected` before
  submission. The pre-check is best-effort: if the tip is unavailable
  the transaction is submitted anyway and the scheduler enforces expiry
  authoritatively.

If the gateway routes through chuckd, it should not silently degrade to
"accepted by a relay queue." Otherwise the topology choice leaks into
API semantics and makes downstream admission behavior harder to reason
about.

### V1 posture

For testnet v1, the goal is not to solve final-form proposer-ingress economics.
The goal is to establish the correct boundary:

- dedicated public RPC gateway
- private-network hop to validator components
- one explicit admission layer in the gateway
- a submission path that preserves mempool-acceptance semantics

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

`getTransactionStatus` checks two backends in order:

```
1. Scheduler (mempool)
   ├── found → Pending
   └── not found ──→ 2. Indexer
                        ├── committed → Committed { height, block_hash }
                        ├── recent nondeterministic failure → RecentFailure { attemptHeight, conflict }
                        └── not found → Unknown
```

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
| Up | Down | Return `BackendUnavailable` error. Indexer is the authoritative source; partial results are misleading. |
| Down | Down | Return `BackendUnavailable` error. |

`Unknown` is therefore a positive claim — "not in the mempool and not
in the indexer" — only produced when both backends answered.

The indexer is treated as the critical backend. If it's unreachable, the
request fails rather than returning potentially misleading results.

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
| `groundhog_sendRawTransaction(bytes) → TxHash` | configurable submission backend | Preserve mempool-acceptance semantics in both modes |
| `groundhog_getTransactionReceipt(hash) → Receipt` | indexer `get_transaction_info` | No `storage_delta` in Phase 1 |
| `groundhog_getTransactionStatus(hash) → Status` | scheduler `find_tx` + indexer | See lookup order above |
| `groundhog_chainTip() → TipInfo` | scheduler `get_tip` | |
| `groundhog_getBlockByNumber(height) → BlockInfo` | **TBD** | In OpenRPC; handler is not implemented until a block query backend exists |
| `groundhog_getEvents(EventQuery) → EventPage` | indexer `get_events` | Gateway enforces per-page limit and cursor length |
| `groundhog_pendingTransactionCount() → u64` | scheduler `size()` | In OpenRPC; gateway handler is not wired yet |
| `groundhog_getStorageAt(address, key) → StorageItemRead` | DbGateway | Requires `db_gateway` endpoint |
| `groundhog_view(request) → ViewResponse` | ViewGateway | Requires `view_gateway` endpoint |
| `groundhog_version() → VersionInfo` | gateway build metadata | Reports wire-format, crate implementation, and optional platform-release versions |

Plus `rpc.discover` for free from jsonrpsee's OpenRPC generation.

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
