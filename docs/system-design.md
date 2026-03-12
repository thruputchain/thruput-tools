# RPC Gateway: System Design

**Status:** Draft v2 — updated 2026-03-11 with findings from hogd/chuckd-rs code review
**Context:** Follows from [opportunity-developer-facing-api.md](opportunity-developer-facing-api.md) — this is the "how" for Option B (separate Rust gateway service).

---

## Questions for Geoff

These are blocking or near-blocking for Phase 1 implementation. Context
from reviewing the hogd and chuckd-rs codebases is included.

### 1. Block data source for `getBlockByNumber`

chuckd-rs has `DiskStorage::lookup_catchup_block(height)` and
`lookup_cv_by_height(height)`, but these are only accessible through
the catchup/repair peer protocols (`Catchup_Catchup1::catchup_solicit`,
`Repair_Repair1::repair_missing_cv`), which use signed requests and
are designed for peer-to-peer block sync, not general queries.

Options:
- **(A)** Add a lightweight x23 query endpoint to chuckd-rs (e.g.,
  `query_block_by_height`) wrapping `DiskStorage::lookup_catchup_block`.
  Minimal change, clean separation.
- **(B)** Have hogd index block headers during commit and serve them
  from the indexer. More work, but avoids gateway→coordinator dependency.
- **(C)** Defer `getBlockByNumber` from Phase 1 (6 methods instead of 7).

**This is the biggest open question** — option A requires chuckd-rs
changes, option B requires hogd changes.

### 2. Method prefix

`groundhog_`, `gh_`, or something else? Low stakes but should be
decided before clients exist. Current scaffold uses `groundhog_`.

### 3. Byte format for JSON wire protocol

All byte values (hashes, addresses, event topics, event data) need a
JSON serialization convention. Recommendation: **`0x`-prefixed lowercase
hex**, matching the Ethereum/Alchemy convention.

`groundhog-xdr` already serializes `SHA256Hash`, `ContractAddress`,
`Ed25519PublicKey` as lowercase hex via `serde_arrays::hex`, but
**without** `0x` prefix. The gateway's `api-types` crate defines
wrapper types (`HexBytes`, `Topic`) that add the `0x` prefix for
new fields. The open question is whether to also wrap the XDR
re-exports (`TxHash`, etc.) to add the prefix consistently, or
accept bare hex for hashes and `0x`-hex for event data.

Recommendation: `0x` everywhere for consistency. Skip checksums for
now (can add a display format later like EIP-55 without changing
wire format).

### 4. Notify over x23 for Phase 2

For Phase 2 subscriptions, does exposing the `Notify` interface over
x23 RPC make sense, or should the gateway connect to a dedicated event
stream? [Petar, Geoff]

### Resolved (from code review, 2026-03-11)

- **`storage_delta` in receipts:** Not available from hogd indexer.
  `TxInfo` only contains `height` + `TxEventLog` (events). Deferred
  from Phase 1 — removed from `TransactionReceipt`. Add back when
  indexer support lands.
- **`pendingTransactionCount` backend:** Maps to `scheduler.size()`,
  which returns total tx count including runqueue + stranded.
  (`hogd/mempool/scheduler.h:136`)
- **Event structure in hogd:** `TxEmittedEvent.serialized_event`
  concatenates topics + data: `topic0 (32B) || ... || data (rest)`.
  `num_topics` (0–3) determines the split point. Max total 1024 bytes.
  Gateway must parse this. (`hogd/xdr/event.hh`)
- **Hash serialization:** `groundhog-xdr` serializes `SHA256Hash`,
  `ContractAddress`, `Ed25519PublicKey` as lowercase hex strings via
  `serde_arrays::hex`. No `0x` prefix from the XDR layer.
  (`protocol-rs/groundhog-xdr/src/types.rs:345`)

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
                   │        groundhog-rpc gateway      │
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
| Contract events by address + height range | hogd indexer | `get_contract_events(start, end, addr)` | No built-in limits — gateway must enforce bounds |
| Chain tip (height, hash) | hogd scheduler | `get_tip()` | |
| Transaction submission | hogd scheduler (via coordinator) | `submit_unvetted(batch)` | |
| Transaction lookup (pending) | hogd scheduler | `lookup_transactions(hashes)` | Returns `Option<SignedTransaction>` per hash |
| Pending tx count | hogd scheduler | `size()` | Atomic counter, includes runqueue + stranded |
| Block/CV data by height or hash | chuckd coordinator | **TBD** — see question #1 | `DiskStorage` exists but not exposed for queries |

### What Needs x23 Exposure

Most query paths already exist. Gaps:

- **Block data by height/hash**: `DiskStorage.lookup_cv_by_height()`
  and `lookup_catchup_block()` exist in chuckd-rs but are only
  accessible through the catchup/repair peer protocols, which use
  signed requests. A lightweight query endpoint is needed, OR hogd
  needs to index block headers. **[Question #1 above]**

- **Block commit notifications**: `Notify` interface exists (chuckd)
  but is stubbed (`IgnoreNotify`). Phase 2 needs this wired to a
  broadcast channel that the gateway can subscribe to.
  **[Question #4 above]**

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

**Note on `groundhog-xdr`:** The XDR crate serializes hash/address types
as bare hex (no `0x` prefix) via `serde_arrays::hex`. The `api-types`
crate's wrapper types (`HexBytes`, `Topic`) add the prefix. Whether to
also wrap XDR re-exports for prefix consistency is an open question
(see question #3).

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
                        ├── found → Committed { height, block_hash }
                        └── not found → Unknown
```

**Degraded-mode behavior:**

| Scheduler | Indexer | Behavior |
|-----------|---------|----------|
| Up | Up | Full status (Pending / Committed / Unknown) |
| Down | Up | Can return Committed or Unknown but cannot detect Pending. Log warning, do not fail the request. |
| Up | Down | Return `BackendUnavailable` error. Indexer is the authoritative source; partial results are misleading. |
| Down | Down | Return `BackendUnavailable` error. |

The indexer is treated as the critical backend. If it's unreachable, the
request fails rather than returning potentially misleading results.

---

## Event Query Bounds

Hogd's `get_contract_events(start, end, addr)` has **no built-in limits**
on height range or result count. A single query could return the entire
event history for a contract and OOM the gateway.

The gateway enforces bounds:

- **Max height range:** Configurable, default 1000 blocks. Requests
  exceeding this return `RANGE_TOO_LARGE` (-32011).
- **Validation:** `to_height >= from_height` (else `RANGE_TOO_LARGE`).

Phase 4 adds cursor-based pagination for large result sets.

---

## Crate Structure

```
groundhog-rpc/                          NEW workspace
├── .cargo/config.toml                  private registry config
├── Cargo.toml                          workspace root
├── crates/
│   ├── api-types/                      shared RPC types
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── hex.rs                  HexBytes, Topic (0x-hex serde)
│   │   │   ├── receipt.rs              TransactionReceipt, ExecStatus
│   │   │   ├── block.rs               BlockInfo, ChainTip
│   │   │   ├── transaction.rs          TxHash, TransactionStatus
│   │   │   ├── events.rs              EmittedEvent, IdentifiedEvent
│   │   │   └── error.rs               RPC error codes
│   │   └── Cargo.toml                  deps: groundhog-xdr, serde, jsonrpsee-types
│   │
│   ├── gateway/                        JSON-RPC server binary
│   │   ├── src/
│   │   │   ├── main.rs                 tokio::main, CLI, startup
│   │   │   ├── config.rs              TOML config + CLI args
│   │   │   ├── context.rs             shared state (x23 clients, metrics)
│   │   │   ├── api.rs                 jsonrpsee trait definition (#[rpc(server)])
│   │   │   ├── handlers/
│   │   │   │   ├── submit.rs          sendRawTransaction
│   │   │   │   ├── receipt.rs         getTransactionReceipt, getTransactionStatus
│   │   │   │   ├── blocks.rs          chainTip, getBlockByNumber
│   │   │   │   └── events.rs          getContractEvents
│   │   │   ├── middleware.rs          timeout, metrics, CORS
│   │   │   └── error.rs              GatewayError → jsonrpsee ErrorObject
│   │   └── Cargo.toml                 deps: api-types, rpc_driver, jsonrpsee, axum, tokio
│   │
│   └── client/                         typed JSON-RPC client library
│       ├── src/
│       │   └── lib.rs                  generated from same #[rpc(client)] trait
│       └── Cargo.toml                  deps: api-types, jsonrpsee-client
```

### Dependency Flow

```
groundhog-xdr  (private registry, from protocol-rs)
xdrs           (private registry)
rpc_driver     (private registry, x23 RPC framework)
       │
       ▼
api-types      (this workspace — shared by all consumers)
       │
   ┌───┴───┐
   ▼       ▼
gateway   client
   │       │
   │       └──→ SDK (sdk_rs), CLI tool, block explorer
   │
   └──→ deployed as standalone service
```

### Type Unification

`api-types` is the shared layer between `groundhog-xdr` (wire format)
and what developers see. It re-exports XDR types where appropriate
and defines RPC-specific types (receipts, status enums, error codes).

**Consumed by:**
- `gateway` (server-side serialization)
- `client` (client-side deserialization)
- `sdk_rs` (via `client` — `GroundhogClient` wraps it)
- CLI tool (via `client`)
- Block explorer (via `client`)

**Not in `api-types`:** contract-execution types (those stay in
`groundhog-xdr` and `sdk`), consensus internals, SCP types.

### Client Crate Trait Location

The `#[rpc(client, server)]` trait in `gateway/src/api.rs` generates
both `GroundhogApiServer` (for the gateway) and `GroundhogApiClient`
(for the client crate). The client crate needs access to the generated
client code, which means one of:

- **(A)** Move the trait to `api-types` so both `gateway` and `client`
  can import it. Pro: clean dependency graph. Con: `api-types` gains
  a `jsonrpsee` proc-macro dependency.
- **(B)** Keep the trait in `gateway` and have `client` depend on
  `gateway` (with a feature flag to avoid pulling in the server).
  Pro: trait stays near handler code. Con: awkward dependency direction.
- **(C)** Duplicate the trait definition in `client` with only
  `#[rpc(client)]`. Pro: no cross-dependency. Con: two definitions
  to keep in sync.

**Recommendation:** **(A)** — move the trait to `api-types`. The
jsonrpsee macro dependency is lightweight, and it keeps the single
source of truth for the API surface in the shared crate.

### Private Registry Dependencies

```toml
# .cargo/config.toml
[registries.groundhog]
index = "..."  # private registry URL

# Cargo.toml (workspace root)
[workspace.dependencies]
groundhog-xdr = { version = "0.3", registry = "groundhog" }
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

Seven RPC methods (or six if `getBlockByNumber` is deferred — see
question #1). All reads via x23 to hogd indexer/scheduler. HTTP
transport only (no WebSocket).

| Method | Data Source | Notes |
|--------|-----------|-------|
| `groundhog_sendRawTransaction(bytes) → TxHash` | scheduler `submit_unvetted` | |
| `groundhog_getTransactionReceipt(hash) → Receipt` | indexer `get_transaction_info` | No `storage_delta` in Phase 1 |
| `groundhog_getTransactionStatus(hash) → Status` | scheduler `find_tx` + indexer | See lookup order above |
| `groundhog_chainTip() → TipInfo` | scheduler `get_tip` | |
| `groundhog_getBlockByNumber(height) → BlockInfo` | **TBD** (question #1) | May defer |
| `groundhog_getContractEvents(addr, from, to) → Events` | indexer `get_contract_events` | Gateway enforces height range limit |
| `groundhog_pendingTransactionCount() → u64` | scheduler `size()` | Includes runqueue + stranded |

Plus `rpc.discover` for free from jsonrpsee's OpenRPC generation.

### Phase 2: Subscriptions (WebSocket)

- `groundhog_subscribeNewBlocks` — committed block stream
- `groundhog_submitAndWatch(bytes)` — tx lifecycle stream
- Requires wiring `Notify` interface to broadcast channel

### Phase 3: SDK Client + CLI

- `client` crate auto-generated from `#[rpc(client)]` trait
- SDK wraps it as `GroundhogClient`
- CLI uses it for `groundhog tx submit`, `groundhog tx status`, etc.

### Phase 4: Production Hardening

- Rate limiting (per-IP token bucket)
- Request size limits
- Per-method Prometheus histograms
- Cursor-based pagination on event/block range queries
- DenyUnsafe for public nodes
- API key auth (optional)
