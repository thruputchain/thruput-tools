# Contract Events Query Scenario (Phase 3, Slice J) - Design

**Date:** 2026-05-29
**Status:** Implemented.
**Owner of implementation:** Codex.

## Summary

Add an `events-query` workload that exercises public
`groundhog_getEvents` reads against contract addresses resolved from
`contractArtifacts` deployed at run startup. This is the next small read-plane
slice after `view-call` and `storage-read`: it proves the simulator can query
the indexer event API from scenario-declared contract filters and record the
observed page metadata in normal workload artifacts.

This slice is read-only after setup. It does not require the smoke contract to
emit events; the first scenario accepts an empty page and validates deploy,
indexer availability, query wiring, pagination cursor shape, and artifact
aggregation.

## Scope

### In scope

- Add manifest field `eventQueries`.
- Add `EventQuerySpec` entries with:
  - `name`;
  - `contract`, referencing `contractArtifacts[].name`;
  - `order`, `ascending` or `descending`;
  - `anchor`, using the public event API's `kind` shape;
  - `cursor`, a 0x-prefixed pagination cursor;
  - `limit`;
  - optional `expectMinEvents`.
- Prepare event-query targets after successful contract deployment by resolving
  artifact names to deployed addresses.
- Implement `events-query` execution through `groundhog_getEvents`.
- Record additive workload event fields for event-query pages.
- Validate runtime `indexer` capability through the existing
  `WorkloadKind::EventsQuery` requirement.
- Add a local Fibonacci events-query smoke scenario and catalog entry.
- Extend scenario smoke coverage when `groundhog-examples` is available.

### Out of scope

- Topic filters; Slice J queries by deployed contract address only.
- Multi-page walking; each workload tick issues one configured page request.
- Event payload/topic decoding or assertions.
- Contract mutation workloads that intentionally emit events during scheduled
  load.
- Committed `.wasm` fixtures.

## Manifest Shape

```json
{
  "workloads": [
    { "name": "events-query", "weight": 1 }
  ],
  "contractArtifacts": [
    { "name": "fibonacci", "wasm": "fibonacci.wasm" }
  ],
  "eventQueries": [
    {
      "name": "fib-events",
      "contract": "fibonacci",
      "order": "descending",
      "anchor": { "kind": "atEnd" },
      "cursor": "0x",
      "limit": 10,
      "expectMinEvents": 0
    }
  ]
}
```

Validation rejects duplicate `eventQueries[].name`, unknown contract
references, non-0x, odd-length, or >256-byte cursors, negative `atHeight`
anchors, limits outside `1..=200`, and `events-query` workloads without at
least one contract artifact and one `eventQueries` entry.

## Execution Semantics

1. Deploy configured contracts before scheduled workload dispatch, unchanged
   from Slice G.
2. Build `PreparedEventQuery` entries by replacing manifest-local contract
   names with deployed contract addresses and decoding cursors.
3. For each scheduled `events-query` tick, use the first prepared event query:
   - filter: one deployed contract address;
   - topics: all positions empty, meaning match any topic;
   - order/anchor/cursor/limit: manifest values;
   - RPC: `groundhog_getEvents`.
4. A successful RPC response records a completed workload unless
   `expectMinEvents` is configured and the page contains fewer events.
5. RPC failures and expectation mismatches record failed workloads and count
   against normal error-rate semantics.

## Artifacts

`result.jsonl` workload events remain schemaVersion 1 and gain optional
additive fields:

- `eventQueryName`;
- `eventContract`;
- `eventCount`;
- `eventCursor`;
- `eventLatestBlock`;
- `eventOldestBlock`.

`rpcMethod` is `groundhog_getEvents`.

`result.json` needs no schema change. Events-query counts, latencies, and error
classes flow through existing per-workload aggregates.

## Example

Add `examples/sim/local-fibonacci-events-query-smoke.json`. The smoke builds
Fibonacci from `groundhog-examples`, deploys it, queries events for the
deployed address, and accepts an empty page. This is intentionally modest: it
validates deploy, runtime indexer availability, RPC wiring, event shape, and
result aggregation before richer event-emitting contracts are introduced.

## Review Gates

- Slice J1: spec review for manifest shape, validation, and artifact fields.
- Slice J2: code review for event-query preparation and executor integration.
- Slice J3: test/artifact review for JSONL shape, catalog/docs, and smoke
  coverage.

Each gate uses two reviewers:

- behavior/API reviewer;
- test/artifact reviewer.

## Verification

- `cargo fmt`
- `cargo test -p groundhog-sim`
- `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
- `cargo run -p groundhog-sim --bin hoglab -- catalog --tag events-query`
- gated: `scripts/smoke-sim-scenarios.sh`
