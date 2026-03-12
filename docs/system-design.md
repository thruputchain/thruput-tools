# RPC Gateway: System Design

**Status:** Draft — for review by Geoff, Daniel
**Context:** Follows from [opportunity-developer-facing-api.md](opportunity-developer-facing-api.md) — this is the "how" for Option B (separate Rust gateway service).

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

| Data | Source | Existing RPC |
|------|--------|-------------|
| Transaction receipt (events, height, status) | hogd indexer | `get_transaction_info(tx_hash)` |
| Contract events by address + height range | hogd indexer | `get_contract_events(start, end, addr)` |
| Chain tip (height, hash) | hogd scheduler | `get_tip()` |
| Transaction submission | hogd scheduler (via coordinator) | `submit_unvetted(batch)` |
| Transaction lookup (pending) | hogd scheduler | `lookup_transactions(hashes)` |
| Block/CV data by height or hash | chuckd coordinator | `DiskStorage` queries (needs x23 exposure) |

### What Needs x23 Exposure

Most query paths already exist. Gaps:

- **Block data by height/hash**: `DiskStorage.lookup_cv_by_height()`
  and `lookup_cv()` exist in chuckd but aren't exposed via x23 RPC
  to external services. Either expose them or have the gateway
  reconstruct block info from the indexer + scheduler.
  **[Question for Geoff: preferred path?]**

- **Block commit notifications**: `Notify` interface exists (chuckd)
  but is stubbed (`IgnoreNotify`). Phase 2 needs this wired to a
  broadcast channel that the gateway can subscribe to.
  **[Question for Petar: any concerns with exposing Notify over x23?]**

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
│   │   │   ├── receipt.rs              GroundhogReceipt, ExecStatus
│   │   │   ├── block.rs               BlockInfo, BlockRange
│   │   │   ├── transaction.rs          TransactionStatus, SubmitResult
│   │   │   ├── events.rs              EventFilter, EmittedEvent
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
│   │   │   └── error.rs              RpcError → jsonrpsee ErrorObject
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

Seven RPC methods. All reads via x23 to hogd indexer/scheduler.
HTTP transport only (no WebSocket).

| Method | Data Source |
|--------|-----------|
| `groundhog_sendRawTransaction(bytes) → TxHash` | scheduler `submit_unvetted` |
| `groundhog_getTransactionReceipt(hash) → Receipt` | indexer `get_transaction_info` |
| `groundhog_getTransactionStatus(hash) → Status` | scheduler `find_tx` + indexer |
| `groundhog_chainTip() → TipInfo` | scheduler `get_tip` |
| `groundhog_getBlockByNumber(height) → BlockInfo` | coordinator storage (needs exposure) |
| `groundhog_getContractEvents(addr, from, to) → Events` | indexer `get_contract_events` |
| `groundhog_pendingTransactionCount() → u64` | scheduler `size()` |

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
- Pagination on event/block range queries
- DenyUnsafe for public nodes
- API key auth (optional)

---

## Open Questions

1. **Block data exposure:** How should the gateway get block/CV data?
   Expose `DiskStorage` queries over x23, or reconstruct from
   indexer + scheduler? **[Geoff]**

2. **Method prefix:** `groundhog_`, `gh_`, or something else?
   **[Geoff, Daniel]**

3. **Receipt type:** The opportunity doc proposes `storage_delta`
   (net HOG locked/unlocked) in receipts. Is that available from
   the indexer today, or does the event service need to capture it?
   **[Geoff, Jack]**

4. **Notify over x23:** For Phase 2 subscriptions, does exposing
   the `Notify` interface over x23 RPC make sense, or should the
   gateway connect to a dedicated event stream? **[Petar, Geoff]**
