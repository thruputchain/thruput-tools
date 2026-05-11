# RPC Gateway: Open Questions and Resolved Decisions

Cross-team open questions and resolved decisions for the gateway design.
Sourced from `system-design.md` to keep the architecture doc focused on
the settled shape.

## Open

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

### ~~3. Byte format for JSON wire protocol~~ — RESOLVED

See "Resolved" section below.

### 4. Notify over x23 for Phase 2

For Phase 2 subscriptions, does exposing the `Notify` interface over
x23 RPC make sense, or should the gateway connect to a dedicated event
stream? [Petar, Geoff]

## Resolved (from code review, 2026-03-11)

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
- **Byte format (Q3):** `0x`-prefixed lowercase hex for all byte values
  (hashes, addresses, topics, event data). Decimal integers for heights
  and counts. Implemented via `Hex0x<T>` wrapper in `api-types/src/hex.rs`
  — wraps XDR byte types at the API boundary without modifying
  `groundhog-xdr`. Bare hex (no prefix) is accepted on input for
  backwards compatibility. (`api-types/src/hex.rs`, `api-types/src/lib.rs`)
