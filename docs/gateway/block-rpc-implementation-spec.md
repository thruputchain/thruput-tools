# Block RPC Design
**Date:** 2026-07-28 **Status:** Ready for `groundhog-rpc` implementation planning. The indexer RPC contract is merged; the hogd ingestion and storage implementation remains a separate follow-up.
## Goal
Support explorer block pages through the hogd indexer and public JSON-RPC gateway. Clients can fetch canonical block metadata by height and paginate the transaction references committed in one block.
## Scope
### In scope
- Implement the existing `groundhog_getBlockByNumber` method against the hogd indexer.

- Add `groundhog_getTransactionsByBlock` with the established indexer pagination model.

- Use the same behavior in the production gateway and local node supervisor.

- Update public types, validation, OpenRPC, metrics, tests, and gateway docs.

- Migrate `chainTip` to the current `DbTip { value: ConsensusValue }` binding without changing its public shape.

- Adapt the local block driver to the current scheduler propose/commit contract required by the coordinated protocol dependency update.

- Define the behavior hogd must provide for end-to-end support.

### Out of scope
- Returning full transaction bodies from the block transaction query. Clients hydrate refs through `groundhog_getTransactionReceipt`.

- Prescribing RocksDB keys, column families, or retention implementation.

- Block-range queries or block subscriptions.

- Implementing the hogd ingestion, storage, or query handlers in this repository.

## Decisions
| # | Decision | Rationale |
|---|---|---|
| D1 | The hogd indexer is the source for both block methods. | Keeps explorer reads on the existing `IndexerBlockExplorer` service and avoids another gateway backend. |
| D2 | `get_block_by_height` returns the canonical `ConsensusValue`. | The current protocol defines the XDR hash of `ConsensusValue` as block identity and embeds the executed `ProposalHeader`. |
| D3 | The public block transaction method accepts one query object. | Matches `getEvents` and `getTransactionsByAddress`, including `order`, `cursor`, and `limit`. |
| D4 | Transaction refs are ordered lexicographically by their 32 unsigned hash bytes in the requested direction. | Provides deterministic pagination without claiming an execution index. |
| D5 | The spec defines required query behavior, not physical indexes. | Hogd maintainers will choose storage that also fits retention and pruning work. |
| D6 | The existing `BlockInfo` schema is replaced before the method becomes functional. | Its legacy `BlockHeader` fields no longer match the consensus protocol. This is a breaking result-schema change and requires a major wire-format bump. |
| D7 | Both block methods live on the shared `ReadPlane`. | Both are indexer reads. This lets the local node reuse its existing indexer client instead of adding a second connection pool or a composite chain backend. |
| D8 | The local block driver mirrors hogd-dev's current proposal construction. | The protocol migration replaces nominate/commit with propose/commit and requires a canonical `ConsensusValue`; matching hogd avoids a divergent local chain model. |

## Merged Indexer Contract

The following contract is merged in `groundhog-protocol` and published in the Rust nightly bindings.

### Block By Height

```xdr
enum BlockLookupOutcome {
  BlockFound = 0,
  BlockNotFound = 1
};

union BlockLookupResult switch (BlockLookupOutcome outcome) {
case BlockFound:
  ConsensusValue value;
default:
  void;
};

BlockLookupResult get_block_by_height(BlockHeight) = 2;
```

### Transactions By Block

```xdr
struct BlockTransactionQuery {
  BlockHeight height;
  IterationOrder order;
  PaginationCursor cursor;
  unsigned int limit;
};

TransactionRefPage get_transactions_by_block(BlockTransactionQuery) = 6;
```

`TransactionRefPage` contains `{ tx_hash, height }` refs, an opaque cursor, and the indexer's current `latest_block` and `oldest_block` bounds.
## Public JSON-RPC
### Block By Number
Keep the existing method signature:

```ts
groundhog_getBlockByNumber(height): BlockInfo | null
```

Replace the legacy `BlockHeader` projection with a JSON-friendly projection of the returned `ConsensusValue`:

```ts
type BlockInfo = {
  blockHash: string;
  protocol: number;
  prevBlockHash: string;
  networkHash: string;
  proposer: string;
  blockTimepoint: string;
  blockHeight: number;
  transactionsHash: string;
  storageHash: string;
  eventsHash: string;
  proposalTransactionsHash: string;
  upgrades: string[];
};
```

Serialization rules:

- `blockHash` is `SHA256(canonical XDR(ConsensusValue))`.

- `blockTimepoint` is Unix time in nanoseconds, encoded as a decimal string to preserve the full `u64` value in JavaScript clients. Its schema pattern is `^[0-9]+$`.

- Hashes, validator IDs, and upgrades use lowercase `0x`-prefixed hex.

- Current zero-valued XDR extension points are omitted from the public shape.


Outcome mapping:

- `BlockFound` returns `BlockInfo`.

- `BlockNotFound`, including unknown, future, or no-longer-retained heights, returns `null`.

- Unknown union outcomes and indexer transport or storage failures map to the existing backend-unavailable error.

- A returned value whose embedded `block_height` differs from the requested height is a backend-contract violation and maps to backend unavailable.

- Negative heights are rejected as invalid params before backend dispatch.

### Transactions By Block
Add:

```ts
groundhog_getTransactionsByBlock({
  height: number;
  order: "ascending" | "descending";
  cursor: string;
  limit: number;
}): TransactionRefPage
```

Semantics:

- Return only transaction refs committed at the requested height.

- Reject a negative query height as invalid params before backend dispatch.

- Sort refs lexicographically over the 32 unsigned signed-transaction-hash bytes, ascending or descending according to `order`.

- `cursor: "0x"` starts a query. A non-empty cursor continues the same height query and carries the iteration order, so the request's `order` is ignored.

- Malformed, stale, or height-mismatched cursors return an empty page with an empty cursor, matching existing indexer pagination behavior.

- `limit: 0` selects the indexer default of 50. Positive values above the lower of the configured transaction-page cap and the protocol ceiling of 200 are rejected as invalid params by the JSON-RPC server.

- The configured transaction-page cap must remain within `50..=200`, so the indexer default cannot exceed the advertised server cap.

- Cursors above the configured byte limit are rejected as invalid params before backend dispatch.

- Existing empty blocks and unknown, future, or no-longer-retained heights all return an empty page. Callers can use `getBlockByNumber` and page bounds when they need to distinguish these cases.

- Every successful page, including an empty page, reports the indexer's current `latestBlock` and `oldestBlock`.

- Before the indexer has established valid retained bounds, the query fails as backend unavailable; `0/0` must not falsely claim that block zero is indexed.

- A page containing a ref for another height is a backend-contract violation and maps to backend unavailable; the gateway must not filter malformed refs because doing so would invalidate cursor semantics.

- Backend failures return backend unavailable rather than an empty page. This feature does not add a new per-call indexer timeout policy.

## RPC Architecture
- Move block lookup from `ChainPlane` to `ReadPlane` and add transactions-by-block beside transactions-by-address.

- The production gateway and full local-node read adapters reuse their existing indexer clients. Read backends without an indexer return `NotImplemented`.

- A production gateway whose optional indexer is absent or disconnected returns backend unavailable. A local runtime that never advertised the indexer capability returns not implemented, matching existing capability semantics. Public behavior is otherwise identical when the capability exists.

- Reuse `TransactionRefPage` and shared transaction pagination validation.

- Use coordinated protocol version `0.20.1-nightly.20260728.g0561b96` for all Groundhog protocol crates, `rpc_driver 0.10.0`, and `xdrs 0.8.1`.

- Adapt `chainTip` to derive `height` from `DbTip.value.proposal_header.block_height` and `blockHash` from `SHA256(canonical XDR(DbTip.value))`. This hash must match `BlockInfo.blockHash` for the same consensus value.

- Adapt the local block driver to call `schedulercoord_propose_block` with an explicitly zero-valued `ProposalContext`, matching current hogd-dev behavior, then commit a `ConsensusValue` with protocol `0`, the parent value's canonical hash, the returned proposal header, the canonical `ProposalTxs` compatibility hash, and no upgrades. Implement the compatibility hash from hogd's documented algorithm and verify it against hogd's golden vector; plain SHA-256 of the complete `ProposalTxs` is incorrect.

- Add the new method to the bounded metrics label set and OpenRPC error mapping.

- Remove `BlockNotFound` from `getBlockByNumber`'s OpenRPC errors because an ordinary miss is represented by `null`.

- Bump `WIRE_FORMAT_VERSION` from `2.3.1` to `3.0.0` because `BlockInfo` changes incompatibly. The new method alone would otherwise be a minor addition.

## Hogd Requirements
Hogd must:

- retain the canonical `ConsensusValue` for each retained, indexed height;

- preserve direct transaction lookup by hash;

- retain each transaction ref only while its transaction record remains available for hydration;

- paginate transaction refs for one exact height without scanning the complete global transaction index;

- keep block metadata, transaction refs, and the contiguous indexed-block watermark consistent across ingestion and restart;

- expose only committed data at or below that watermark;

- return no successful page until a valid oldest/latest retained range exists;

- implement the not-found, cursor, ordering, limit, bounds, and failure semantics defined above.


The current `IndexerEvent` ingestion RPC carries height, event logs, and transaction bodies, but not `ConsensusValue`. The hogd follow-up must choose the committed-data handoff into the indexer. If that handoff changes an internal RPC, it requires a focused protocol follow-up; it does not change the approved query shape above.
## Repository Responsibilities
1. `groundhog-protocol`: query contract, merged; any later ingestion-only wire change if hogd requires one.

2. `groundhog-protocol-rs`: generated Rust bindings, published as a coordinated nightly.

3. `groundhog-rpc`: public types and methods, gateway and local-node adapters, validation, OpenRPC, metrics, tests, and documentation.

4. `groundhog-hogd`: committed-value ingestion, persistent storage, efficient queries, retention behavior, and end-to-end tests.

## Verification
The `groundhog-rpc` work must cover:

- `BlockInfo` projection, decimal-string timepoint, and canonical block-hash vectors;

- `chainTip` projection and hash consistency with `getBlockByNumber`;

- local propose/commit behavior and the upstream `ProposalTxs` compatibility hash golden vector;

- found, not-found, negative-height, mismatched-height, unknown-outcome, and transport-failure block lookups;

- ascending and descending transaction pages;

- first, continuation, exhausted, empty-block, invalid-cursor, height-mismatched-cursor, and malformed-ref-height behavior;

- limit and cursor-size validation before backend dispatch;

- production gateway and local-node behavior through mocked indexer clients;

- API trait, typed client, conformance, OpenRPC snapshot, and metrics labels;

- formatting, clippy, focused crate tests, and the required workspace gate.


Real end-to-end block reads remain gated on the hogd implementation.
## Acceptance Criteria
- A retained, indexed height returns the documented `BlockInfo`, including the canonical block hash.

- An unknown, future, or no-longer-retained height returns `null` from `getBlockByNumber`.

- Block transaction queries return only refs from the requested height in the requested deterministic order.

- Pagination and page bounds follow the merged indexer contract.

- Invalid params fail before backend dispatch; backend failures are not presented as empty results.

- Production gateway and local node return the same public behavior.

- OpenRPC and `groundhog_version.wireFormat` identify the new API shape.

- Hogd restart preserves block and transaction lookup results once its implementation lands.
