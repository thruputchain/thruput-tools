# RPC Gateway: API Sketch

**Status:** Draft — for Daniel to review and iterate
**Context:** Code sketches for [rpc-gateway-system-design.md](rpc-gateway-system-design.md).
Not compilable as-is; intended to communicate shape and decisions.

---

## 1. jsonrpsee Trait Definition

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

    /// Lightweight status check. Queries mempool first (Pending),
    /// then indexer (Committed), falls back to Unknown.
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

    /// Number of transactions in the mempool.
    #[method(name = "pendingTransactionCount")]
    async fn pending_transaction_count(&self) -> RpcResult<u64>;

    // --- Event Queries ---

    /// Contract events filtered by address and height range.
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

## 2. API Types (`api-types` crate)

These are the RPC-layer types that developers interact with.
They import from `groundhog-xdr` where the wire type already
exists and define new types where the RPC needs something different.

```rust
// api-types/src/lib.rs

pub mod receipt;
pub mod block;
pub mod transaction;
pub mod events;
pub mod error;

// Re-export XDR types that appear in the public API unchanged.
pub use groundhog_xdr::types::{
    ContractAddress,
    SHA256Hash,
    BlockHeight,
    Ed25519PublicKey,
};
```

### Transaction Hash

```rust
// api-types/src/transaction.rs

use serde::{Serialize, Deserialize};
use groundhog_xdr::types::SHA256Hash;

/// Canonical transaction hash: sha256(xdr_serialize(signed_tx.tx)).
/// Covers the inner Transaction body (not signatures), matching
/// the scheduler's lookup key and the indexer's primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TxHash(pub SHA256Hash);

/// Lightweight status — the result of checking mempool then indexer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TransactionStatus {
    /// In the mempool, not yet in a committed block.
    Pending,

    /// Included in a committed block. SCP finality = done.
    Committed {
        height: BlockHeight,
        block_hash: SHA256Hash,
    },

    /// Scheduler rejected the transaction.
    Invalid {
        reason: String,
    },

    /// Not found in mempool or indexer.
    Unknown,
}

/// Result of sendRawTransaction.
/// Phase 1: just the hash. Phase 2 response options could add
/// wait_for_commit, show_events, etc. (Sui pattern).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResult {
    pub tx_hash: TxHash,
}
```

### Receipt

```rust
// api-types/src/receipt.rs

use serde::{Serialize, Deserialize};
use groundhog_xdr::types::{BlockHeight, SHA256Hash, ContractAddress};
use crate::transaction::TxHash;
use crate::events::EmittedEvent;

/// Full transaction receipt. Returned by getTransactionReceipt.
///
/// Diverges from EVM receipts intentionally:
/// - No gasUsed/gasPrice (gas bounds computation but costs zero)
/// - No transactionIndex (not stored in indexer)
/// - No logsBloom (events are directly indexed)
/// - ExecStatus is richer than EVM's binary 0/1
/// - storage_delta replaces gas economics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    pub tx_hash: TxHash,
    pub height: BlockHeight,
    pub status: ExecStatus,
    pub events: Vec<EmittedEvent>,

    /// Net change in locked HOG from storage writes/deletes.
    /// Positive = more HOG locked (storage grew).
    /// Negative = HOG unlocked (storage shrank).
    /// None if not available from the indexer.
    /// [TODO: confirm with Geoff whether this is in TxEventLog today]
    pub storage_delta: Option<i128>,
}

/// Execution result. Maps to hogd's ExecResult variants.
/// Richer than EVM's binary status (0 = fail, 1 = success).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum ExecStatus {
    /// Transaction executed successfully.
    Ok,

    /// Root call frame reverted (contract called abort/revert).
    Reverted {
        /// Abort buffer from the contract, if any.
        message: Option<String>,
    },

    /// Deterministic execution error (out of gas, stack overflow, etc.).
    DeterministicError {
        kind: String,
    },

    /// Gas sponsorship failed.
    FailedGasSponsorship,

    // Future: additional variants as hogd ExecResult evolves.
    // Keep this enum non_exhaustive so clients handle new variants.
    #[serde(other)]
    Other,
}
```

### Block Info

```rust
// api-types/src/block.rs

use serde::{Serialize, Deserialize};
use groundhog_xdr::types::{BlockHeight, SHA256Hash};
use crate::transaction::TxHash;

/// Current chain tip. Returned by chainTip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainTip {
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
}

/// Block summary. Returned by getBlockByNumber.
///
/// Intentionally minimal for Phase 1. Doesn't include full
/// transaction list (that's a separate query if needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockInfo {
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
    pub parent_hash: SHA256Hash,
    pub tx_count: u32,
    pub tx_hashes: Vec<TxHash>,
    // TODO: timestamp? CV bytes? consensus metadata?
    // Keep minimal until we know what block explorers need.
}
```

### Events

```rust
// api-types/src/events.rs

use serde::{Serialize, Deserialize};
use groundhog_xdr::types::{BlockHeight, SHA256Hash, ContractAddress};
use crate::transaction::TxHash;

/// A single emitted event, as stored by the hogd indexer.
/// Maps to hogd's TxEmittedEvent + identification metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedEvent {
    pub contract_address: ContractAddress,

    /// Up to 3 topics per event (indexed, filterable in Phase 4).
    pub topics: Vec<Vec<u8>>,

    /// Opaque event data (up to 1KB, contract-defined encoding).
    pub data: Vec<u8>,
}

/// Event with provenance — includes which tx emitted it.
/// Returned by getContractEvents.
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
/// We follow Substrate's pattern of per-module ranges.
///
/// -32000 to -32009: Transaction errors
/// -32010 to -32019: Query errors
/// -32020 to -32029: Subscription errors (Phase 2)
pub mod codes {
    /// Transaction was malformed (bad XDR, missing fields).
    pub const TX_INVALID: i32 = -32000;

    /// Transaction was well-formed but rejected by the scheduler.
    pub const TX_REJECTED: i32 = -32001;

    /// Transaction hash not found in mempool or indexer.
    pub const TX_NOT_FOUND: i32 = -32002;

    /// Block height out of range (negative, or beyond current tip).
    pub const BLOCK_NOT_FOUND: i32 = -32010;

    /// Height range too large for getContractEvents.
    pub const RANGE_TOO_LARGE: i32 = -32011;

    /// Backend (hogd indexer or scheduler) unavailable.
    pub const BACKEND_UNAVAILABLE: i32 = -32050;

    /// Request timed out waiting for backend response.
    pub const BACKEND_TIMEOUT: i32 = -32051;
}
```

---

## 3. Gateway Context (Shared State)

```rust
// gateway/src/context.rs

use std::sync::Arc;

/// Shared state injected into every handler.
/// All backend connections are behind Arc for cheap cloning
/// into spawned handler tasks.
pub struct GatewayContext {
    /// x23 RPC client to hogd scheduler.
    /// Used for: submit_unvetted, get_tip, find_tx, size.
    pub scheduler: Arc<SchedulerClient>,

    /// x23 RPC client to hogd indexer.
    /// Used for: get_transaction_info, get_contract_events.
    pub indexer: Arc<IndexerClient>,

    /// x23 RPC client to coordinator storage.
    /// Used for: lookup_cv_by_height, lookup_cv.
    /// [TODO: may not be needed if block data comes from indexer]
    pub coordinator: Option<Arc<CoordinatorClient>>,

    /// Prometheus metrics registry.
    pub metrics: Arc<GatewayMetrics>,

    /// Server configuration (timeouts, limits, etc.).
    pub config: Arc<GatewayConfig>,
}
```

---

## 4. Handler Example

```rust
// gateway/src/handlers/receipt.rs

use crate::context::GatewayContext;
use groundhog_rpc_api_types::{
    transaction::TxHash,
    receipt::TransactionReceipt,
};

/// Implementation of groundhog_getTransactionReceipt.
///
/// Calls hogd indexer's get_transaction_info(tx_hash) via x23 RPC.
/// Converts TxEventLog → TransactionReceipt for the RPC response.
pub async fn get_transaction_receipt(
    ctx: &GatewayContext,
    tx_hash: TxHash,
) -> RpcResult<Option<TransactionReceipt>> {
    // 1. Query hogd indexer
    let tx_info = ctx.indexer
        .get_transaction_info(tx_hash.0)
        .await
        .map_err(|e| internal_error(e, "indexer query failed"))?;

    // 2. Convert to API type
    let Some(info) = tx_info else {
        return Ok(None);
    };

    // 3. Map TxEventLog events to EmittedEvent
    let events = info.event_log.events
        .iter()
        .map(|e| EmittedEvent {
            contract_address: e.contract_address,
            topics: e.topics(),
            data: e.data(),
        })
        .collect();

    // 4. Map ExecResult to ExecStatus
    let status = map_exec_result(&info.event_log.exec_result);

    Ok(Some(TransactionReceipt {
        tx_hash,
        height: info.height,
        status,
        events,
        storage_delta: None, // TODO: available from indexer?
    }))
}
```

---

## 5. Workspace Cargo.toml

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
groundhog-xdr = { version = "0.3", registry = "groundhog" }
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
anyhow = "1"
```

```toml
# crates/api-types/Cargo.toml
[package]
name = "groundhog-rpc-api-types"
version = "0.1.0"
edition = "2021"

[dependencies]
groundhog-xdr = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
jsonrpsee-types = { workspace = true }
bytes = { workspace = true }
hex = { workspace = true }
```

```toml
# crates/gateway/Cargo.toml
[package]
name = "groundhog-rpc-gateway"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "groundhog-rpc"
path = "src/main.rs"

[dependencies]
groundhog-rpc-api-types = { path = "../api-types" }
groundhog-xdr = { workspace = true }
rpc_driver = { workspace = true }
jsonrpsee = { workspace = true }
axum = { workspace = true }
tokio = { workspace = true }
tower = { workspace = true }
tower-http = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
prometheus = { workspace = true }
clap = { workspace = true }
toml = { workspace = true }
sha2 = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
```

```toml
# crates/client/Cargo.toml
[package]
name = "groundhog-rpc-client"
version = "0.1.0"
edition = "2021"

[dependencies]
groundhog-rpc-api-types = { path = "../api-types" }
jsonrpsee = { workspace = true, features = ["client", "macros"] }
tokio = { workspace = true }
serde = { workspace = true }
```

---

## Notes for Daniel

- The `#[rpc(client, server)]` macro on the trait generates both
  `GroundhogApiServer` (for the gateway) and `GroundhogApiClient`
  (for the client crate / SDK / CLI). One definition, two sides.

- `api-types` should be the single source of truth for every type
  that crosses the RPC boundary. The SDK's `GroundhogClient` wraps
  `GroundhogApiClient` and re-exports these types.

- Serde rename conventions: `camelCase` for JSON field names
  (matches EVM/Sui conventions). Tag-based enum serialization
  for `TransactionStatus` and `ExecStatus` (discriminated unions
  in JSON).

- `ExecStatus` variants should track hogd's `ExecResult` enum.
  I've sketched the obvious ones; the full mapping needs your
  and Geoff's input on which variants are user-visible vs internal.

- `TxHash` is `sha256(xdr_serialize(signed_tx.tx))` — the inner
  `Transaction` body, not the full `SignedTransaction`. This matches
  the scheduler's canonical key and the indexer's primary key.
  The hash computation should live in `api-types` (or a utility
  in `groundhog-xdr`) so the SDK and gateway compute it identically.

- I've left `storage_delta` as `Option<i128>` on the receipt.
  Whether it's available depends on what the indexer's `TxEventLog`
  captures today — question for Geoff/Jack.

- Topics on events: hogd supports up to 3 topics × 32 bytes
  (similar to EVM log topics). Phase 4 adds topic-based filtering
  in the indexer (needs a third RocksDB column family). For now
  they're returned but not filterable.

- jsonrpsee version: 0.24 is current as of early 2026. Both
  Substrate and Sui use it. Needs to be on the private registry
  (or mirrored from crates.io).
