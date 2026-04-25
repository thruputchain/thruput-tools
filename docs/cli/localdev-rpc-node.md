# Local Dev Node

How the dev node works, what it fakes, what it shares with
production. For CLI and RPC gateway development.

**Date:** 2026-04-08

---

## Status Update: Backend Strategy

This document describes the first local dev-node prototype: a JSON-RPC
server backed by the SDK `native_exec` / hogd `test_ffi` path.

As of 2026-04-24, the durable backend direction is documented in
`docs/cli/specs/2026-04-24-embedded-dev-node-backend.md` and
`docs/cli/plans/2026-04-24-embedded-dev-node-backend.md`.

The Rust-side aperture that ties the dev-node to the chuckd-rs coordinator,
the gateway, and any future embedded backend is captured in
[`2026-04-24-dev-node-layering.md`](2026-04-24-dev-node-layering.md). That
companion doc proposes consuming `chuckd_scheduler::SchedulerClient` as
the dev-node's backend trait, with three swappable implementations (TCP
against `hogd --no-chuckd`, embedded over libhogdev, and a testkit mock).

The important distinction: the prototype is not a mock executor. It reaches
the real hogd execution machinery. The weak point is the test-harness
lifecycle contract: current `test_ffi` lacks durable local-node semantics
such as snapshot/revert, time travel, and arbitrary historical state reads.
Future work should replace the backend aperture with a hogd-side embedded
execution library, while keeping this prototype useful for near-term DX
experiments.

## Goal

An RPC-accessible local node for `groundhog deploy`, `groundhog
call`, and interactive development. No remote testnet needed.

## Execution Levels

| Level | Description | Startup | Fidelity |
|-------|------------|---------|----------|
| **A** | `TestEnvironment` in-process, no RPC | 0ms | Execution only |
| **B** | `native_exec` behind jsonrpsee | ~100ms | Execution + RPC + serialization |
| **C** | Real gateway → hogd → coordinator | Seconds+ | Production-faithful |

**A exists** — SDK unit tests use it today.

**B was the first prototype target** — same executor, exposed over the
standard `GroundhogApi` JSON-RPC interface. This is the backend that
proved the Anvil-style DX surface is valuable, but it should not ossify as
the durable backend aperture.

**C is out of scope here** — needed eventually for CI and
pre-deploy validation, not for the dev loop.

The durable target is the embedded hogd execution library described in
`docs/cli/specs/2026-04-24-embedded-dev-node-backend.md`.

## What Is Real vs Simulated

| Component | Real? | Notes |
|-----------|-------|-------|
| RPC interface | Yes | Same `GroundhogApi` trait, same `api-types` |
| XDR deserialization | Yes | Full `SignedTransaction` decode |
| Auth verification | Yes | `verify_signed_transaction_auth` |
| Contract execution | Yes | `native_exec`, identical to tests |
| Storage / deltas | Yes | Same executor DB, same merge semantics |
| Storage cost model | Yes | Same `StorageCostModel` |
| Block advancement | Simulated | Auto-advance per tx, or on-demand |
| Mempool | None | Execute immediately, no ordering |
| Indexing | Minimal | In-memory receipts and events |
| Consensus | None | Single node, instant commit |
| Chain tip | Simulated | Monotonic counter |

The simulated parts are exactly the parts that are trivial in
Groundhog (instant finality, no gas auctions, no reorgs). The
real parts are the parts that affect correctness.

## Why B Was Enough For The Prototype

- Snapshot isolation and delta merges happen in the executor. B
  is faithful.
- No gas fees — nothing to estimate or fake. Storage tokens are
  tracked by the executor.
- Instant finality — no confirmation delay. Committed is
  committed.
- No reorgs — receipts are final immediately.

Level C is only needed for concurrent submission ordering,
multi-validator consensus, indexer load, and admission control.
None of these matter much during development.

What B does not provide is a supported long-lived local-node lifecycle:
snapshot/revert, time travel, durable block controls, and honest historical
state reads still need a hogd-side embedded backend.

## Where the Code Lives

The dev backend depends on the gateway's RPC trait and the SDK's
`native_exec`. Three options:

**Option 1: In the gateway repo.** Gateway gets a `--dev` flag.
`DevBackend` sits alongside the real hogd backend.

- Pro: RPC trait stays in one place, tested alongside the real
  gateway.
- Con: gateway gains a dep on `native_exec` + groundhog-sys
  (C++ FFI). Heavy.

**Option 2: In the CLI repo.** CLI embeds a jsonrpsee server.
Single binary.

- Pro: single binary, zero setup.
- Con: duplicates the RPC trait, harder to keep in sync.

**Option 3: Gateway owns it, CLI wraps it.** The
gateway repo owns `DevBackend` and `--dev`. The CLI's `groundhog
node` spawns the gateway in dev mode. Later, the CLI can embed
it behind a feature flag if single-binary DX justifies the build
cost.

## DevBackend Shape

```
DevBackend {
    env: TestEnvironmentHandle,
    receipts: HashMap<TxHash, TransactionReceipt>,
    events: Vec<IdentifiedEvent>,
    tip_height: BlockHeight,
    block_txs: Vec<Vec<TxHash>>,
    config: DevConfig,
}
```

## RPC Method Mapping

**`sendRawTransaction(bytes) → TxHash`** — Deserialize XDR →
verify auth → register `contracts_to_deploy` → execute via
`TransactionHandler` → store receipt → advance block → return
hash. Synchronous commit by default.

**`getTransactionReceipt(hash)`** — Look up in-memory receipts
map.

**`getTransactionStatus(hash)`** — Committed if in receipts,
Unknown otherwise. No Pending state (execution is synchronous).

**`chainTip()`** — Current height + synthetic block hash.

**`getBlockByNumber(height)`** — Synthetic block info if height
exists.

**`pendingTransactionCount()`** — Always 0.

**`getContractEvents(addr, from, to)`** — Filter in-memory event
store. Enforce `RANGE_TOO_LARGE` so client code hits the same
limit in dev and production.

## Dev-Only RPC Methods

Under a `groundhog_dev_` namespace:

- `advanceBlock()` — advance without submitting a transaction
- `reset()` — wipe all state
- `setStorageTokens(addr, amount)` — grant storage tokens
- `getAccounts()` — list pre-funded accounts and keys

Return errors if called against a non-dev node.

## Pre-Funded Accounts

Deterministic accounts from a well-known seed, using the same
`derive_keypair` pattern as `tx-builder`:

```
Seed: "groundhog-dev-accounts"
Account N: derive("groundhog-dev-accounts", "account-{N}")
```

Each account gets a deployed pubkey-account contract and storage
tokens. Print the table on startup with a warning not to use the
keys on a real network.

## State Persistence

```bash
groundhog node                        # ephemeral
groundhog node --state ./state.json   # load/save
groundhog node --reset                # ignore saved state
```

The executor DB is backed by C++ (groundhog-sys). Dump/restore
may require a new FFI API. Defer if hard — ephemeral-only is
fine for v1.

## CLI Integration

```bash
groundhog node                  # localhost:9545
groundhog node --port 9546
groundhog node --state state.json
groundhog node --reset
groundhog node --show-deltas
groundhog node --show-events
groundhog node --accounts 20
```

When `deploy` or `call` can't connect to a localhost URL:

```
error: could not connect to http://localhost:9545

  hint: run `groundhog node` in another terminal
```

Don't auto-start. `groundhog.toml` defaults to `localhost:9545`
so everything works if the node is running.

## Confirmed: Executor Supports the Full Path

### Contract deployment via transaction

The C++ executor handles `contracts_to_deploy` on
`SignedTransaction`. `TransactionHandler::new()` passes the full
XDR to `open_syscall_handler`; the executor processes it as a
unit.

However, `contracts_to_deploy` must be registered via
`setup_create_contract` before the transaction runs — the
executor needs the WASM blob available when the deploy syscall
fires. In production hogd does this during block preparation. In
the dev node, do it inline before calling `root_invoke`.

```rust
let stx: SignedTransaction = xdrs::txs::decode(&tx_bytes)?;
env.verify_signed_transaction_auth(&stx);

for contract in &stx.tx.contracts_to_deploy {
    env.setup_create_contract(contract);
}

let handler = TransactionHandler::new(env.clone(), &stx);
let result = handler.root_invoke();
let exec_result = handler.try_commit_transaction();
env.advance_block();
```

Confirmed by `bottle-test`, which runs cross-contract WASM calls
through this path.

### Real WASM execution

`TestEnvironment.setup_create_contract` handles
`Contract::kWasmi` (real WASM) alongside `Contract::kMockTest`
(mock closures). See `test_environment.rs:231-236`.

`sdk::test_wasm()`, `bottle-test`, and the example contract WASM
tests all exercise real WASM execution through the full
transaction pipeline. No executor changes needed.

## Open Questions

1. **Gateway dep on native_exec** — Is it acceptable for the
   gateway's dev feature to pull in the C++ FFI layer?

2. **State persistence** — Does a dump/restore API exist on the
   FFI boundary, or does it need to be added?

3. **Block timing** — Support `--block-time <secs>` for
   timer-based advancement, or is advance-on-transaction enough?

4. **Single binary** — Worth the build cost of embedding
   native_exec + groundhog-sys in the CLI behind a feature flag?
