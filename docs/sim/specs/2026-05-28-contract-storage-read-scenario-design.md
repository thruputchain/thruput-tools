# Contract Storage Read Scenario (Phase 3, Slice I) - Design

**Date:** 2026-05-28
**Status:** Implemented.
**Owner of implementation:** Codex.

## Summary

Add a `storage-read` workload that exercises public
`groundhog_getStorageAt` reads against contract addresses resolved from
`contractArtifacts` deployed at run startup. This is the next small
contract-state slice after deploy plus `view-call`: it proves the simulator can
drive the DB/read plane with scenario-declared contract/key pairs and record the
observed committed snapshot in normal workload artifacts.

This slice is read-only after setup. It does not add a state-mutating contract
workload; the first smoke reads a known key from the deployed Fibonacci
contract and expects absence.

## Scope

### In scope

- Add manifest field `storageReads`.
- Add `StorageReadSpec` entries with:
  - `name`;
  - `contract`, referencing `contractArtifacts[].name`;
  - `key`, a 32-byte 0x-prefixed storage key;
  - optional `expectPresent`.
- Prepare storage-read targets after successful contract deployment by
  resolving artifact names to deployed addresses.
- Implement `storage-read` execution through `groundhog_getStorageAt`.
- Record additive workload event fields for storage reads.
- Validate runtime `dbGateway` capability through the existing
  `WorkloadKind::StorageRead` requirement.
- Add a local Fibonacci storage-read smoke scenario and catalog entry.
- Extend scenario smoke coverage when `groundhog-examples` is available.

### Out of scope

- Contract mutation workloads that intentionally write storage during scheduled
  load.
- Decoding storage payloads beyond item presence and item kind.
- Multiple storage-read selection policies; Slice I uses the first configured
  entry, matching Slice G `view-call` selection.
- Storage assertions beyond per-read `expectPresent`.
- Committed `.wasm` fixtures.
- `events-query` execution.

## Manifest Shape

```json
{
  "workloads": [
    { "name": "storage-read", "weight": 1 }
  ],
  "contractArtifacts": [
    { "name": "fibonacci", "wasm": "fibonacci.wasm" }
  ],
  "storageReads": [
    {
      "name": "fib-zero-key",
      "contract": "fibonacci",
      "key": "0x0000000000000000000000000000000000000000000000000000000000000000",
      "expectPresent": false
    }
  ]
}
```

Validation rejects duplicate `storageReads[].name`, unknown contract
references, non-0x or non-32-byte keys, and `storage-read` workloads without at
least one contract artifact and one `storageReads` entry.

## Execution Semantics

1. Deploy configured contracts before scheduled workload dispatch, unchanged
   from Slice G.
2. Build `PreparedStorageRead` entries by replacing manifest-local contract
   names with deployed contract addresses and decoding keys.
3. For each scheduled `storage-read` tick, use the first prepared storage read:
   - address: deployed contract address;
   - key: manifest key bytes;
   - RPC: `groundhog_getStorageAt`.
4. A successful RPC response records a completed workload unless
   `expectPresent` is configured and does not match `item != null`.
5. RPC failures and expectation mismatches record failed workloads and count
   against normal error-rate semantics.

## Artifacts

`result.jsonl` workload events remain schemaVersion 1 and gain optional
additive fields:

- `storageReadName`;
- `storageContract`;
- `storageKey`;
- `storageItemPresent`;
- `storageItemKind`.

The existing `chainHeight` field records the storage snapshot
`blockHeight`. `rpcMethod` is `groundhog_getStorageAt`.

`result.json` needs no schema change. Storage-read counts, latencies, and error
classes flow through existing per-workload aggregates.

## Example

Add `examples/sim/local-fibonacci-storage-read-smoke.json`. The smoke builds
Fibonacci from `groundhog-examples`, deploys it, reads the zero key, and expects
no item to be present. This is intentionally modest: it validates deploy,
runtime DB-plane availability, RPC wiring, event shape, and result aggregation.

## Review Gates

- Slice I1: spec review for manifest shape, validation, and artifact fields.
- Slice I2: code review for storage-read preparation and executor integration.
- Slice I3: test/artifact review for JSONL shape, catalog/docs, and smoke
  coverage.

Each gate uses two reviewers:

- behavior/API reviewer;
- test/artifact reviewer.

## Verification

- `cargo fmt`
- `cargo test -p groundhog-sim`
- `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
- `cargo run -p groundhog-sim --bin hogsim -- catalog --tag storage-read`
- gated: `scripts/smoke-sim-scenarios.sh`
