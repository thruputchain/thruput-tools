# RPC Gateway: API Sketch

**Status:** Draft v2 — updated 2026-03-11
**Context:** Code sketches for [system-design.md](system-design.md).
Not compilable as-is; intended to communicate shape and decisions.

---

## 1. Wire Format Conventions

All byte values use `0x`-prefixed lowercase hex. Numeric values use
JSON numbers (decimal). See system-design.md "Wire Format Conventions"
for the full table.

```json
{
  "txHash": "0xa1b2c3d4e5f6...",
  "height": 42,
  "status": { "result": "ok" },
  "events": [
    {
      "contractAddress": "0x00112233...",
      "topics": ["0xdeadbeef...", "0xcafebabe..."],
      "data": "0x48656c6c6f"
    }
  ]
}
```

---

## 2. jsonrpsee Trait Definition

Single trait, `#[rpc(client, server)]` generates both the gateway
server stubs and the typed Rust client library. This is the Substrate
pattern (jsonrpsee proc macros). The `client` crate gets a working
`GroundhogApiClient` for free.

```rust
use jsonrpsee::proc_macros::rpc;
use crate::types::*;

/// Phase 1: Core API — submit, query, poll.
/// Phase 2 adds subscription methods below.
#[rpc(client, server, namespace = "groundhog")]
pub trait GroundhogApi {

    // --- Transaction Submission ---

    /// Submit a signed transaction. Returns the canonical tx hash
    /// (sha256 of the inner Transaction body, not SignedTransaction).
    /// Fire-and-forget: returns immediately after mempool acceptance.
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, tx_bytes: Bytes) -> RpcResult<TxHash>;

    // --- Transaction Queries ---

    /// Full receipt for a committed transaction.
    /// Returns null if the tx is unknown or still pending.
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>>;

    /// Lightweight status check.
    /// Lookup order: scheduler (Pending) → indexer (Committed) → Unknown.
    /// See system-design.md "Transaction Status Lookup Order" for
    /// degraded-mode behavior when a backend is unavailable.
    #[method(name = "getTransactionStatus")]
    async fn get_transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<TransactionStatus>;

    // --- Chain State ---

    /// Current chain tip: height + block hash.
    #[method(name = "chainTip")]
    async fn chain_tip(&self) -> RpcResult<ChainTip>;

    /// Block data at a given height.
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        height: BlockHeight,
    ) -> RpcResult<Option<BlockInfo>>;

    /// Number of transactions in the mempool (scheduler.size()).
    #[method(name = "pendingTransactionCount")]
    async fn pending_transaction_count(&self) -> RpcResult<u64>;

    // --- Event Queries ---

    /// Contract events filtered by address and height range.
    /// Returns RANGE_TOO_LARGE (-32011) if height span exceeds
    /// the gateway's configured maximum (default: 1000 blocks).
    #[method(name = "getContractEvents")]
    async fn get_contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> RpcResult<Vec<IdentifiedEvent>>;

    // --- Phase 2: Subscriptions (WebSocket) ---

    // /// Stream of committed blocks. Since SCP finality is immediate,
    // /// this serves as both newHeads and newFinalizedHeads.
    // #[subscription(
    //     name = "subscribeNewBlocks" => "newBlock",
    //     unsubscribe = "unsubscribeNewBlocks",
    //     item = BlockInfo,
    // )]
    // async fn subscribe_new_blocks(&self);
    //
    // /// Submit a tx and stream its lifecycle.
    // /// Accepted → Committed | Invalid | Expired.
    // #[subscription(
    //     name = "submitAndWatch" => "txStatus",
    //     unsubscribe = "unwatchTx",
    //     item = TransactionStatus,
    // )]
    // async fn submit_and_watch(&self, tx_bytes: Bytes);
}
```

---

## 3. API Types (`api-types` crate)

These are the RPC-layer types that developers interact with.
They import from `groundhog-xdr` where the wire type already
exists and define new types where the RPC needs something different.

```rust
// api-types/src/lib.rs

pub mod hex;
pub mod receipt;
pub mod block;
pub mod transaction;
pub mod events;
pub mod error;

// Re-export XDR types that appear in the public API unchanged.
pub use groundhog_xdr::types::{
    BlockHeader,
    BlockHeight,
    ContractAddress,
    Ed25519PublicKey,
    SHA256Hash,
};
```

### Hex Byte Types

```rust
// api-types/src/hex.rs

/// Variable-length byte array. Serializes as "0x..." hex string.
/// Used for event data and other opaque byte fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexBytes(pub Vec<u8>);
// Custom Serialize/Deserialize: "0x" + hex::encode / hex::decode

/// Fixed-size 32-byte event topic. Serializes as "0x..." hex string.
/// Hogd supports up to 3 topics per event, each exactly 32 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Topic(pub [u8; 32]);
// Custom Serialize/Deserialize: "0x" + hex::encode / hex::decode
```

### Transaction Hash

```rust
// api-types/src/transaction.rs

/// Canonical transaction hash: sha256(xdrs::txs::encode(signed_tx.tx)).
/// Covers the inner Transaction body (not signatures), matching
/// the scheduler's lookup key and the indexer's primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TxHash(pub SHA256Hash);

/// Lightweight status — the result of checking mempool then indexer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TransactionStatus {
    Pending,
    Committed { height: BlockHeight, block_hash: SHA256Hash },
    Invalid { reason: String },
    Unknown,
}
```

### Receipt

```rust
// api-types/src/receipt.rs

/// Full transaction receipt. Returned by getTransactionReceipt.
///
/// Diverges from EVM receipts intentionally:
/// - No gasUsed/gasPrice (gas bounds computation but costs zero)
/// - No transactionIndex (not stored in indexer)
/// - No logsBloom (events are directly indexed)
/// - ExecStatus is richer than EVM's binary 0/1
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    pub tx_hash: TxHash,
    pub height: BlockHeight,
    pub status: ExecStatus,
    pub events: Vec<EmittedEvent>,
    // storage_delta deferred — not available from hogd indexer.
    // TxInfo only contains height + TxEventLog. Add back when
    // indexer captures storage deltas.
}

/// Execution result. Maps to hogd's ExecResult variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum ExecStatus {
    Ok,
    Reverted { message: Option<String> },
    DeterministicError { kind: String },
    FailedGasSponsorship,
    #[serde(other)]
    Other,
}
```

### Block Types

```rust
// api-types/src/block.rs

/// Current chain tip. Returned by chainTip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainTip {
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
}

/// Block data returned by getBlockByNumber.
/// Wraps the XDR BlockHeader so the RPC layer can add fields
/// without modifying the wire type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockInfo {
    #[serde(flatten)]
    pub header: BlockHeader,
}
```

### Events

```rust
// api-types/src/events.rs

/// A single emitted event.
///
/// The gateway parses hogd's TxEmittedEvent.serialized_event into
/// separate topics and data. See system-design.md "Event Parsing"
/// for the wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedEvent {
    pub contract_address: ContractAddress,
    /// Up to 3 topics per event, each 32 bytes.
    pub topics: Vec<Topic>,
    /// Opaque event data (contract-defined encoding).
    pub data: HexBytes,
}

/// Event with provenance — includes which tx emitted it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifiedEvent {
    pub tx_hash: TxHash,
    pub block_height: BlockHeight,
    pub sequence_number: u32,
    pub event: EmittedEvent,
}
```

### Error Codes

```rust
// api-types/src/error.rs

/// Groundhog-specific JSON-RPC error codes.
/// Standard JSON-RPC reserves -32000 to -32099 for server errors.
/// Per-module ranges following Substrate's pattern.
///
/// -32000 to -32009: Transaction errors
/// -32010 to -32019: Query errors
/// -32020 to -32029: Subscription errors (Phase 2)
/// -32050 to -32059: Backend errors
pub mod codes {
    pub const TX_INVALID: i32 = -32000;
    pub const TX_REJECTED: i32 = -32001;
    pub const TX_NOT_FOUND: i32 = -32002;
    pub const BLOCK_NOT_FOUND: i32 = -32010;
    pub const RANGE_TOO_LARGE: i32 = -32011;
    pub const BACKEND_UNAVAILABLE: i32 = -32050;
    pub const BACKEND_TIMEOUT: i32 = -32051;
}
```

---

## 4. Gateway Context (Shared State)

```rust
// gateway/src/context.rs

/// Shared state injected into every handler.
/// All backend connections are behind Arc for cheap cloning.
pub struct GatewayContext {
    /// x23 RPC client to hogd scheduler.
    /// Used for: submit_unvetted, get_tip, lookup_transactions, size.
    pub scheduler: Arc<groundhog_scheduler::SchedulerCoord_SchedulerCoord1_Client>,

    /// x23 RPC client to hogd indexer.
    /// Used for: get_transaction_info, get_contract_events.
    pub indexer: Arc<IndexerClient>,

    /// x23 RPC client to coordinator storage (optional).
    /// Used for: block/CV queries and any future chuckd-backed submit path.
    ///
    /// Note: protocol-rs currently provides a generated scheduler client, but
    /// not an equivalent generated x23 unvetted-submit path into chuckd.
    pub coordinator: Option<Arc<CoordinatorClient>>,

    /// Prometheus metrics registry.
    pub metrics: Arc<GatewayMetrics>,

    /// Server configuration (timeouts, limits, etc.).
    pub config: Arc<GatewayConfig>,
}
```

---

## 5. Handler Examples

### getTransactionReceipt

```rust
pub async fn get_transaction_receipt(
    ctx: &GatewayContext,
    tx_hash: TxHash,
) -> RpcResult<Option<TransactionReceipt>> {
    // 1. Query hogd indexer
    let tx_info = ctx.indexer
        .get_transaction_info(tx_hash.0)
        .await
        .map_err(|e| internal_error(e, "indexer query failed"))?;

    let Some(info) = tx_info else { return Ok(None); };

    // 2. Parse serialized_event → topics + data
    let events = info.event_log.events
        .iter()
        .map(|e| {
            let (topics, data) = parse_serialized_event(
                e.num_topics,
                &e.serialized_event,
            );
            EmittedEvent {
                contract_address: e.contract_address,
                topics,
                data,
            }
        })
        .collect();

    // 3. Map ExecResult → ExecStatus
    let status = map_exec_result(&info.event_log.exec_result);

    Ok(Some(TransactionReceipt {
        tx_hash,
        height: info.height,
        status,
        events,
    }))
}
```

### getTransactionStatus (lookup order)

```rust
pub async fn get_transaction_status(
    ctx: &GatewayContext,
    tx_hash: TxHash,
) -> RpcResult<TransactionStatus> {
    // 1. Check scheduler (mempool) — best-effort
    match ctx.scheduler.lookup_transactions(&[tx_hash.0]).await {
        Ok(results) if results[0].is_some() => {
            return Ok(TransactionStatus::Pending);
        }
        Err(e) => {
            // Scheduler down — log warning, continue to indexer
            tracing::warn!(error = %e, "scheduler unreachable, cannot detect pending txs");
        }
        _ => {}
    }

    // 2. Check indexer — authoritative
    let tx_info = ctx.indexer
        .get_transaction_info(tx_hash.0)
        .await
        .map_err(|e| internal_error(e, "indexer query failed"))?;

    match tx_info {
        Some(info) => Ok(TransactionStatus::Committed {
            height: info.height,
            block_hash: info.block_hash,
        }),
        None => Ok(TransactionStatus::Unknown),
    }
}
```

---

## 6. Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/api-types",
    "crates/gateway",
    "crates/client",
]

[workspace.dependencies]
# --- Groundhog (private registry) ---
groundhog-xdr = { version = "0.3.5", registry = "groundhog" }
xdrs = { version = "0.5", registry = "groundhog" }
rpc_driver = { version = "0.3", registry = "groundhog" }

# --- JSON-RPC ---
jsonrpsee = { version = "0.24", features = ["server", "client", "macros"] }
jsonrpsee-types = "0.24"

# --- HTTP / Async ---
axum = "0.8"
tokio = { version = "1", features = ["full"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace"] }
hyper = "1"

# --- Serialization ---
serde = { version = "1", features = ["derive"] }
serde_json = "1"
bytes = "1"
hex = "0.4"

# --- Observability ---
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
prometheus = "0.13"

# --- CLI / Config ---
clap = { version = "4", features = ["derive"] }
toml = "0.8"

# --- Crypto ---
sha2 = "0.10"

# --- Error handling ---
thiserror = "2"
```

---

## Notes for Daniel

- The `#[rpc(client, server)]` macro on the trait generates both
  `GroundhogApiServer` (for the gateway) and `GroundhogApiClient`
  (for the client crate / SDK / CLI). One definition, two sides.
  See system-design.md "Client Crate Trait Location" for where the
  trait should live.

- `api-types` should be the single source of truth for every type
  that crosses the RPC boundary. The SDK's `GroundhogClient` wraps
  `GroundhogApiClient` and re-exports these types.

- Serde rename conventions: `camelCase` for JSON field names
  (matches EVM/Sui conventions). Tag-based enum serialization
  for `TransactionStatus` and `ExecStatus` (discriminated unions
  in JSON).

- Both `TransactionStatus` and `ExecStatus` are `#[non_exhaustive]`
  so new variants can be added without semver breaks. Clients must
  handle unknown variants via `#[serde(other)]` on `ExecStatus` and
  by matching a wildcard arm on `TransactionStatus`.

- `ExecStatus` variants should track hogd's `ExecResult` enum.
  The full mapping needs your and Geoff's input on which variants
  are user-visible vs internal.

- `TxHash` is `sha256(xdrs::txs::encode(signed_tx.tx))` — the inner
  `Transaction` body, not the full `SignedTransaction`. This matches
  the scheduler's canonical key and the indexer's primary key.
  The hash computation should live in `api-types` (or a utility
  in `groundhog-xdr`) so the SDK and gateway compute it identically.

- `storage_delta` deferred from Phase 1 — hogd's `TxInfo` doesn't
  include it. Will add to `TransactionReceipt` when indexer support
  lands.

- Topics on events: hogd supports up to 3 topics × 32 bytes
  (similar to EVM log topics). Phase 4 adds topic-based filtering
  in the indexer (needs a third RocksDB column family). For now
  they're returned but not filterable.

- jsonrpsee version: 0.24 is current as of early 2026. Both
  Substrate and Sui use it.
