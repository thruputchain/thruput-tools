# RPC Gateway: Open Questions and Resolved Decisions

Cross-team open questions and resolved decisions for the gateway design.
Sourced from `system-design.md` to keep the architecture doc focused on
the settled shape.

## Open

### 1. Notify over x23 for Phase 2

For Phase 2 subscriptions, does exposing the `Notify` interface over
x23 RPC make sense, or should the gateway connect to a dedicated event
stream? [Petar, Geoff]

## Resolved

- **Block query backend:** The hogd `IndexerBlockExplorer` is the source for
  both `getBlockByNumber` and `getTransactionsByBlock`. The production gateway
  and full local-node adapter use their existing optional indexer clients. The
  RPC implementation is complete; real block data still requires hogd to ingest
  and persist canonical `ConsensusValue` records and per-block transaction refs.

The remaining decisions below came from code review on 2026-03-11.

- **`storage_delta` in receipts:** Not available from hogd indexer.
  `TxInfo` only contains `height` + `TxEventLog` (events). Deferred
  from Phase 1 — removed from `TransactionReceipt`. Add back when
  indexer support lands.
- **`pendingTransactionCount` backend:** Maps to `scheduler.size()`,
  which returns total tx count including runqueue + stranded. The public method
  is in the OpenRPC surface; the gateway handler remains unwired until the
  scheduler client path is plumbed through. (`hogd/mempool/scheduler.h:136`)
- **Method prefix:** The public namespace is `groundhog_*`. Local dev-account
  discovery is intentionally not an RPC namespace; current dev-key metadata
  discovery goes through runtime manifests.
- **Event structure in hogd:** `TxEmittedEvent.serialized_event`
  concatenates topics + data: `topic0 (32B) || ... || data (rest)`.
  `num_topics` (0–3) determines the split point. Max total 1024 bytes.
  Gateway must parse this. (`hogd/xdr/event.hh`)
- **Hash serialization:** `groundhog-xdr` serializes `SHA256Hash`,
  `ContractAddress`, `Ed25519PublicKey` as lowercase hex strings via
  `serde_arrays::hex`. No `0x` prefix from the XDR layer.
  (`protocol-rs/groundhog-xdr/src/types.rs:345`)
- **Byte format (Q3):** `0x`-prefixed lowercase hex for all byte values
  (hashes, addresses, topics, event data). Decimal integers for heights
  and counts. Implemented via `Hex0x<T>` wrapper in
  `crates/json-rpc/json-rpc-types/src/hex.rs`
  — wraps XDR byte types at the API boundary without modifying
  `groundhog-xdr`. Bare hex (no prefix) is accepted on input for
  backwards compatibility. (`json-rpc-types/src/hex.rs`,
  `json-rpc-types/src/lib.rs`)
