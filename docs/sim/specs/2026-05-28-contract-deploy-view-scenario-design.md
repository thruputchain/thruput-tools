# Contract Deploy + View Scenario (Phase 3, Slice A) - Design

**Date:** 2026-05-28
**Status:** Draft for implementation.
**Owner of implementation:** Codex.

## Summary

Add the first contract-aware simulator scenario by deploying a compiled example
contract at run startup and exercising it through the existing `view-call`
workload placeholder. This slice uses the `fibonacci` contract from the sibling
`groundhog-examples` repository as the initial richer workload source while
keeping the simulator schema explicit and artifact-driven.

The goal is to put local target, deployment, view gateway, result artifacts, and
scenario catalog tooling through a real contract path without taking on event
query, storage read, cross-contract calls, or typed ABI encoding in the same
slice.

## Scope

This is **Slice G** of the simulator work, the first Phase 3 contract-state
slice after the scenario catalog. It extends runtime execution beyond
`chain-tip`, `wallet-submit-smoke`, and `receipt-poll` by making `view-call`
executable when the manifest declares a deployed contract and a raw view call.

### In scope

- Add manifest fields for compiled contract artifacts and named view calls.
- Add a `hogsim run --contract-artifact-root` override so committed scenarios
  can refer to portable artifact filenames while smoke scripts build examples
  into a temporary directory.
- Deploy declared contract artifacts synchronously after target resolution and
  before scheduled workload ticks.
- Build deploy transactions from simulator actor state using the same wallet
  deploy semantics as `groundhog deploy`.
- Poll the deploy receipt and resolve the deployed contract address from receipt
  return data, falling back to deterministic local address derivation when the
  receipt omits it.
- Implement `view-call` by sending `groundhog_view` to the deployed contract
  with raw entrypoint/calldata configured by the scenario.
- Record deployment JSONL events and final `result.json.contracts` records.
- Record view-call workload output status, height, emitted event count, and
  optional return-data expectation failures in workload events.
- Add a local Fibonacci view smoke scenario and extend the scenario smoke script
  to build `groundhog-examples/fibonacci` before running it.
- Update README and implementation tracker.

### Out of scope

- Typed ABI or method-name encoding in simulator manifests.
- Contract source checkout, build orchestration, or dependency management inside
  `hogsim run`; scripts may build examples before invoking hogsim.
- `events-query` and `storage-read` execution.
- Cross-contract examples such as `cross-call`.
- Stateful mutation workloads beyond the deploy transaction itself.
- Remote SSH provider contract artifact transport.
- Automatic refresh of committed result baselines.

## Manifest Additions

The artifact-neutral contract artifact shape is a breaking manifest/result/event
schema change under `schemaVersion: 2`.

```json
{
  "contractArtifacts": [
    {
      "name": "fibonacci",
      "artifact": {
        "kind": "wasm",
        "path": "fibonacci.wasm"
      }
    }
  ],
  "viewCalls": [
    {
      "name": "fib-10",
      "contract": "fibonacci",
      "entrypoint": "0x...",
      "calldata": "0x...",
      "expectReturnData": "0x..."
    }
  ],
  "workloads": [
    { "name": "view-call", "weight": 1 }
  ]
}
```

- `contractArtifacts[].name` is a stable manifest-local identifier.
- `contractArtifacts[].artifact.kind` is `wasm` or `rv32`.
- `contractArtifacts[].artifact.path` is resolved relative to
  `--contract-artifact-root` when supplied, otherwise relative to the scenario
  file directory.
- `viewCalls[].contract` references a deployed `contractArtifacts[].name`.
- `viewCalls[].entrypoint` must be exactly 8 bytes.
- `viewCalls[].calldata` is raw CXS calldata encoded outside the simulator.
- `viewCalls[].expectReturnData` is optional. When present, a mismatch records a
  failed workload with `errorClass: "view-return-mismatch"`.
- Slice G supports one or more view call definitions, but workload selection may
  choose the first definition deterministically. Weighted selection between
  multiple view-call definitions is deferred until workload parameters are
  schema-backed.

Validation must reject duplicate artifact names, duplicate view-call names,
`viewCalls[].contract` values that do not reference a declared artifact, missing
`viewCalls` when `view-call` appears in `workloads`, malformed hex, entrypoint
values that are not exactly 8 bytes, and `view-call` manifests without a
declared contract artifact.

The Fibonacci scenario must not guess an entrypoint by ASCII padding. Its
committed raw constants must be generated from the built contract metadata using
the same canonical entrypoint derivation and CXS argument/return encoding used
by `groundhog view` and `groundhog call`.

## Deployment Semantics

Deployment runs once per declared artifact, before the generated schedule starts.

1. Load and hash the contract artifact.
2. Wrap it as a `groundhog_xdr::contract::Contract::kWasmi` or
   `Contract::kRv32` blob based on artifact kind.
3. Compute the contract hash as `sha256(xdr_encode(contract))`.
4. Set `tx_nonce = actor0.current_nonce()`.
5. Set `deploy_address_nonce = sha256(tx_nonce.to_le_bytes())`.
6. Derive the predicted address from `(actor0.wallet_address,
   deploy_address_nonce, contract_hash)`.
7. Build wallet deploy calldata with CXS encoding of `(deploy_address_nonce,
   contract_hash)`.
8. Submit the signed deploy transaction through `groundhog_sendRawTransaction`
   using `tx_nonce` as the transaction nonce.
9. Poll `groundhog_getTransactionReceipt` until a receipt is available or the
   existing receipt drain timeout expires.
10. On `ExecStatus::Ok`, resolve the contract address from receipt `return_data`
   if present, otherwise use the locally derived address.
11. Advance actor 0's nonce exactly once per submitted deploy transaction so
   scheduled workloads cannot reuse the deploy nonce.

Nonce advancement must use a deploy-specific helper that only consumes the
nonce. It must not call `ActorState::record_submitted_tx`, because deploy
receipts are polled synchronously and must not enter scheduled receipt-poll
queues or workload metrics.

The lifecycle insertion point is explicit:

1. Resolve target, load runtime, validate manifest/runtime, derive actors,
   generate the schedule, prepare the result directory, and write initial events.
2. Deploy declared artifacts and write deployment JSONL events.
3. If deployment succeeds, enter scheduled workload dispatch.
4. If deployment fails, skip scheduled dispatch, write a terminal
   `result.json` with `outcome: "failed"`, write a `run-completed` JSONL event
   with zero completed/skipped scheduled workloads and at least one failed
   deploy record, then return `RunFailed`.

## View-Call Semantics

`view-call` prepares a `groundhog_view` request from the first configured view
call:

- `contract` is the deployed address for `viewCalls[].contract`.
- `entrypoint` and `calldata` are raw bytes from manifest hex.
- `caller` is actor wallet address for the scheduled actor.

Successful `RootInvocationOutput::Return` counts as a successful workload unless
`expectReturnData` is present and mismatched. `Abort`, `Fault`, RPC failure, and
configuration errors count as workload failures with stable error classes.

The workload event remains backward compatible and adds optional fields for:

- `viewResult`
- `viewReturnData`
- `viewAbortData`
- `viewEventCount`

`viewResult` values are `return`, `abort`, or `fault`, matching
`RootInvocationOutput`. A returned snapshot height uses the existing
`chainHeight` workload-event field. Error classes are stable:
`view-aborted`, `view-faulted`, `view-rpc-error`, `view-config-error`, and
`view-return-mismatch`.

`viewReturnData` and `viewAbortData` are `0x`-prefixed hex strings. They are
present only for `return` and `abort`, respectively. `viewEventCount` is an
integer count present whenever the RPC returns a view response. A
`view-return-mismatch` failure still records `viewResult: "return"`, the actual
`viewReturnData`, `viewEventCount`, and an error message naming the expected
return data.

## Result Artifacts

Additive JSONL events:

- `contract-deploy-started`
- `contract-deploy-completed`
- `contract-deploy-failed`

Add `result.json.contracts`, defaulting to `[]`, with records containing:

- `name`
- `artifactKind`
- `artifactPath`
- `artifactSha256`
- `contractHash`
- `txHash`
- `nonce`
- `status`
- `address`
- `addressSource`
- `height`
- `error`

Required fields in every contract record: `name`, `status`, and `artifactPath`
when the path was resolved. Optional fields are omitted, not written as `null`.
Stage-specific fields are required once available: after artifact load,
`artifactSha256`; after contract wrapping, `contractHash`; after deploy transaction
build, `nonce` and `deployAddressNonce`; after submit, `txHash`; after receipt,
`height`; after successful address resolution, `address` and `addressSource`.
Pre-load artifact failures therefore produce a failed record with `name`,
`status: "failed"`, optional `artifactPath`, and `error`, but no hashes or nonce.

- `artifactKind` is `wasm` or `rv32`.
- `artifactPath` is the resolved absolute or run-working-directory path used by the
  simulator, not the manifest-relative string.
- `artifactSha256`, `contractHash`, `txHash`, and `address` are `0x`-prefixed hex.
- `nonce` is a decimal string containing `tx_nonce`.
- `deployAddressNonce` is optional `0x`-prefixed hex and is recorded when the
  deploy transaction is built.
- `status` values are `started`, `completed`, or `failed`.
- `addressSource` values are `receipt` or `derived`.
- `height` is recorded only after a committed receipt exists.
- `error` is recorded only for failed deploy records.

Example started event:

```json
{
  "schemaVersion": 2,
  "kind": "contract-deploy-started",
  "name": "fibonacci",
  "status": "started",
  "artifactKind": "wasm",
  "artifactPath": "/tmp/.../fibonacci.wasm",
  "artifactSha256": "0x...",
  "contractHash": "0x...",
  "nonce": "123",
  "deployAddressNonce": "0x..."
}
```

Example completed event:

```json
{
  "schemaVersion": 2,
  "kind": "contract-deploy-completed",
  "name": "fibonacci",
  "status": "completed",
  "artifactKind": "wasm",
  "artifactPath": "/tmp/.../fibonacci.wasm",
  "artifactSha256": "0x...",
  "contractHash": "0x...",
  "nonce": "123",
  "deployAddressNonce": "0x...",
  "txHash": "0x...",
  "address": "0x...",
  "addressSource": "derived",
  "height": 42
}
```

Example failed event:

```json
{
  "schemaVersion": 2,
  "kind": "contract-deploy-failed",
  "name": "fibonacci",
  "status": "failed",
  "artifactKind": "wasm",
  "artifactPath": "/tmp/.../fibonacci.wasm",
  "artifactSha256": "0x...",
  "contractHash": "0x...",
  "nonce": "123",
  "deployAddressNonce": "0x...",
  "txHash": "0x...",
  "height": 42,
  "error": "contract deploy receipt status deterministic-error"
}
```

The result and deployment-event schemas are `schemaVersion: 2`. Tests must cover
new contract results and compare behavior that ignores `contracts` until
baseline comparison semantics are intentionally extended.

## Scenario

Add `examples/sim/local-fibonacci-view-smoke.json`:

- provider: `local`
- bootstrap accounts: `1`
- duration: short local smoke, around 3s
- workload: `view-call`
- contract artifact: `fibonacci.wasm`
- view call: `fibonacci.compute(10)`
- expected return data: CXS-encoded `55`
- assertions: low `maxErrorRate`, view gateway required through existing
  workload capability validation

The scenario smoke script builds the sibling examples repo before running this
scenario:

```sh
groundhog build -p fibonacci --target-dir "$WORKDIR/groundhog-examples-target"
hogsim run examples/sim/local-fibonacci-view-smoke.json \
  --contract-artifact-root "$WORKDIR/groundhog-examples-target/groundhog"
```

If the examples repo is missing, the contract smoke is skipped with a clear
message; the normal catalog remains runnable. If present, the Fibonacci
scenario joins the fast local catalog through the ignored Rust smoke test rather
than being run only by shell. The script passes the built artifact root to that
test through `GROUNDHOG_SIM_CONTRACT_ARTIFACT_ROOT`, and the Rust smoke asserts
`result.json.contracts`, deployment JSONL events, successful `view-call`
workload metrics, and view-specific workload event fields.

## Review Gates

- Review gate G1: design/spec review for manifest shape, deployment semantics,
  and deferred scope.
- Review gate G2: deploy lifecycle review for nonce handling, receipt polling,
  address resolution, artifact errors, and failure cleanup.
- Review gate G3: view workload review for scheduler integration, caller
  choice, output classification, and metrics.
- Review gate G4: script/artifact review for example build UX, result JSONL,
  `result.json.contracts`, and backward compatibility.
- Final review gate: full Slice G code review plus smoke/test coverage review
  before commit.

Each gate uses two reviewers: behavior/lifecycle and test/artifact.

## Verification

- `cargo fmt`
- `cargo test -p groundhog-sim`
- `cargo clippy -p groundhog-sim --all-targets -- -D warnings`
- Gated:
  - `GROUNDHOG_HOGD_DEV_BINARY=/absolute/path/to/hogd-dev scripts/smoke-sim-scenarios.sh`
  - Optional:
    `GROUNDHOG_EXAMPLES_ROOT=/absolute/path/to/groundhog-examples scripts/smoke-sim-scenarios.sh`
