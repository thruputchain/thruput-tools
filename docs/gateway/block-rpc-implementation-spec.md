# Block RPC Implementation Spec
## Goal
Support explorer block pages through the existing hogd indexer and JSON-RPC gateway.
## Architecture
- Hogd remains the source of canonical `BlockHeader` values.
  
- The hogd indexer stores block headers and block-to-transaction references.
  
- New indexer queries use the existing `IndexerBlockExplorer` service and listener.
  
- `groundhog-rpc` forwards public JSON-RPC calls to the indexer.
  
## Protocol
Add two queries to `IndexerBlockExplorer`.
### Block By Height
```xdr
enum BlockLookupOutcome {
  BlockFound = 0,
  BlockNotFound = 1
};

union BlockLookupResult switch (BlockLookupOutcome outcome) {
case BlockFound:
  BlockHeader header;
default:
  void;
};

BlockLookupResult get_block_by_height(BlockHeight) = 2;
```
### Transactions By Block
```xdr
struct BlockTransactionQuery {
  BlockHeight height;
  PaginationCursor cursor;
  unsigned int limit;
};

TransactionRefPage get_transactions_by_block(BlockTransactionQuery) = 6;
```

Return lightweight `{ txHash, height }` references ordered by signed transaction hash for deterministic pagination.

Extend committed-block ingestion so the indexer receives the canonical `BlockHeader` with the block's transaction set.
## Hogd Indexer
Keep the existing transaction-hash index and add two persistent indexes:

```text
existing: transaction hash -> transaction record
block height -> serialized BlockHeader
block height || transaction hash -> empty
```

Requirements:

- Persist block metadata and transaction references atomically with existing indexed block data.
  
- Expose only blocks at or below the contiguous indexed-block watermark.
  
- Use a block-height prefix scan for transaction pagination.
  
- Return `BlockNotFound` for unknown or future heights; return storage failures as backend errors.
  
- Preserve data across indexer restarts.
  
## JSON-RPC
### Existing Method
Implement the existing method without changing its public shape:

```ts
groundhog_getBlockByNumber(height): BlockInfo | null
```

- Map `BlockFound` to the existing `BlockInfo` projection.
  
- Map `BlockNotFound` to `null`.
  
- Map indexer transport failures to the existing backend-unavailable error.
  
### New Method
Add:

```ts
groundhog_getTransactionsByBlock(height, {
  cursor?: string;
  limit?: number;
}): TransactionRefPage
```

Use the same behavior in the production gateway and local devnode.
## Repositories
Functional changes:

1. `groundhog-protocol`: wire types and indexer methods.
  
2. `groundhog-hogd`: committed-header ingestion, storage, queries, and tests.
  
3. `groundhog-rpc`: gateway/devnode wiring, public method, OpenRPC, and tests.
  

Generated language bindings and dependency updates follow the normal protocol release workflow.
## Acceptance Criteria
- A known height returns the documented `BlockInfo`.
  
- An unknown or future height returns `null`.
  
- Block transaction queries return only references from the requested block.
  
- Pagination is deterministic and does not scan the global transaction index.
  
- Empty blocks return an empty transaction page.
  
- Production gateway and local devnode behave consistently.
  
- Indexer restart preserves block and transaction lookup results.
