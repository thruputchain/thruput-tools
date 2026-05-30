# Groundhog CLI: DX Roadmap

**Archive note:** Historical competitive analysis from 2026-04-07. This is not
the current CLI roadmap; use [`../../cli/roadmap.md`](../../cli/roadmap.md) for
active planning.

Strategic analysis of Foundry's developer experience patterns applied to
the Groundhog CLI, SDK, and RPC Gateway. Based on source-level review of
Foundry (forge/anvil/cast/chisel), the Groundhog SDK (`sdk_rs`), the
RPC Gateway (`groundhog-rpc`), and the current CLI.

**Date:** 2026-04-07

---

## What Makes Foundry the Champ

Foundry's dominance rests on three architectural bets.

### 1. Four Integrated Binaries, One Shared Core

| Foundry | Purpose | Groundhog Status |
|---------|---------|------------------|
| **forge** | Build, test, deploy, script, debug, coverage, doc, gas reports, watch mode | `groundhog` (build + test only) |
| **anvil** | Local dev node — instant mining, state persistence, forking, account impersonation | **Nothing — biggest gap** |
| **cast** | Swiss-army knife — RPC calls, tx crafting, ABI encoding, unit conversions | `groundhog call` (stubbed) |
| **chisel** | Interactive Solidity REPL | Not planned |

The tools share `foundry-common`, `foundry-evm`, `foundry-config`, and
alloy primitives. A developer never leaves the terminal: write →
`forge build` → `forge test` → `anvil` → `forge script` → `cast`.

### 2. Native-Language Testing + Cheatcodes (~200+)

Foundry's killer insight: **test Solidity in Solidity** (not
JavaScript). The `vm` cheatcode interface gives tests superpowers,
organized across `crates/cheatcodes/src/`:

- **`env.rs`** — `vm.warp()`, `vm.roll()`, `vm.fee()`, `vm.chainId()`,
  `vm.deal()`, `vm.etch()`, `vm.store()`
- **`evm.rs`/`evm/`** — `vm.prank()`, `vm.startPrank()`,
  `vm.snapshot()`, `vm.revertTo()`
- **`test.rs`** — `vm.expectRevert()`, `vm.expectEmit()`,
  `vm.expectCall()`, `vm.assume()`
- **`fs.rs`** — `vm.readFile()`, `vm.writeFile()` (test fixtures)
- **`json.rs`/`toml.rs`** — parse/serialize config in tests
- **`crypto.rs`** — `vm.sign()`, `vm.addr()`, `vm.deriveKey()`
- **`script.rs`** — `vm.broadcast()`, `vm.startBroadcast()`
  (deployment scripting)

### 3. Anvil — The Linchpin

Anvil's `NodeArgs` reveals the full surface:

```
--port 8545           RPC port
--accounts 10         pre-funded accounts
--balance 10000       ETH per account
--block-time 12       interval mining
--no-mining           mine on demand only
--mixed-mining        both interval and on-demand
--state state.json    bidirectional state persistence
--dump-state / --load-state   one-way persistence
--init genesis.json   custom genesis
--hardfork prague     target any hardfork
--order fees          mempool ordering
--prune-history       memory management
--preserve-historical-states  for deep RPC queries
--mnemonic / --mnemonic-seed-unsafe  deterministic accounts
```

Plus custom RPC methods: `anvil_impersonateAccount`, `evm_mine`,
`evm_increaseTime`, `evm_snapshot`, `evm_revert`.

### 4. forge test — Absurdly Rich

From `crates/forge/src/cmd/test/mod.rs`:

```
--debug              step-through debugger
--flamegraph         gas visualization
--flamechart         gas over time
--decode-internal    trace internal functions
--gas-report         per-contract gas breakdown
--gas-snapshot-check compare against baseline
--fuzz-seed / --fuzz-runs / --fuzz-timeout   property testing
--fuzz-input-file    replay specific inputs
--fail-fast          stop on first failure
--rerun              re-run recorded failures
--show-progress      progress bar
--summary / --detailed   summary tables
--junit              CI-friendly output
--suppress-successful-traces
--trace-depth N      control trace verbosity
--list               list tests without running
--watch / --rerun-failed   file watching with TDD mode
```

### 5. Configuration System

`foundry-config` uses **figment** for layered config: `foundry.toml`
profiles + env vars + CLI flags, all composable. Every subcommand
implements `figment::Provider` to override defaults. Profiles
(`[profile.default]`, `[profile.ci]`) switch via `FOUNDRY_PROFILE`
env var.

### 6. Other Notable forge Subcommands

- **`clone`** — clone a verified contract from Etherscan
- **`coverage`** — code coverage
- **`doc`** — generate documentation from NatSpec comments
- **`geiger`** — audit unsafe code in dependencies
- **`inspect`** — ABI, bytecode, storage layout inspection
- **`lint`** — Solidity linting
- **`snapshot`** — gas snapshot comparisons (655 lines of code)
- **`watch`** — `watchexec`-based file watching with debounce,
  rerun-failed, and run-all modes

---

## Current State: Where We Are

### Groundhog CLI (groundhog-cli)

**Implemented:** `new` (counter/stub templates), `build` (WASM
cross-compile), `test` (cargo test with executor creds),
`login`/`logout`, `completion`.

**Stubbed:** `init`, `deploy`, `call`, `keys` (generate/list/use/rm).

**Architecture:** Clean lib/bin split, `Print` for output, `CliError`
with thiserror, XDG config. ~700 LOC, 33 tests. Shells out to cargo
for build/test.

### RPC Gateway (groundhog-rpc)

**Architecture:** 4 crates — `api-types`, `gateway` (jsonrpsee),
`client` (auto-generated from same `#[rpc(client, server)]` trait),
`tx-builder` (E2E testing tool).

**Phase 1 API (7 methods):**

```
groundhog_sendRawTransaction(bytes)          -> TxHash
groundhog_getTransactionReceipt(hash)        -> Option<TransactionReceipt>
groundhog_getTransactionStatus(hash)         -> TransactionStatus
groundhog_chainTip()                         -> ChainTip
groundhog_getBlockByNumber(height)           -> Option<BlockInfo>
groundhog_pendingTransactionCount()          -> u64
groundhog_getEvents(EventQuery)              -> EventPage
```

**Status:** Scaffold-heavy, shaping architecture and types. Not yet a
running service. The `client` crate re-exports `api-types` so
downstream consumers (SDK, CLI) only need one dependency. The
`tx-builder` is a minimal tool that constructs signed transactions
and proves the pattern works.

**Key detail:** Groundhog has no gas fees, instant finality (SCP), no
forks/reorgs. Transaction lifecycle: Pending -> Committed (or
Invalid/Expired). Dramatically simpler than EVM.

### SDK (sdk_rs)

**Test infrastructure** (`native_exec` crate):

- `TestEnvironment` — in-memory blockchain state with mock contracts
- `MockContract` — register Rust closures as contract endpoints
- `MockTransactionHandler` — execute transactions with commit/abort
- `FrameBuilder` / `FrameResult` — fine-grained execution frame control
- `TransactResult` — combined frame + exec result assertions
- `advance_block()` — simulate block progression
- `setup_grant_storage_tokens()` — fund contracts for storage
- `register_mock()` — one-call mock contract registration
- Rich assertion API: `assert_success()`, `assert_abort()`,
  `assert_return_with()`, `assert_event()`, `filter_events()`

The SDK's test environment is the Groundhog equivalent of Foundry's
cheatcodes + EVM in one package. Rust-native testing is already there.

---

## Where Groundhog Can Differentiate

### 1. Snapshot Isolation + Delta Merges = Native Time-Travel

EVM processes transactions sequentially. Foundry *hacks* time-travel
with `vm.snapshot()`/`vm.revertTo()`. Groundhog's execution model is
**natively snapshot-based**: transactions execute against a snapshot
and produce deltas that merge. This enables:

- **Deterministic replay** of any transaction against any snapshot
- **Conflict visualization** — show when/why delta merges conflict
- **Parallel execution exploration** — show the execution tree, not a
  linear chain

The `advance_block()` and `transact()`/`transact_manual()` API already
model this. The CLI can expose it as a first-class debugging experience.

### 2. RetrocausalQueue = Async Contract Patterns

Unique to Groundhog. The DX around understanding, debugging, and
testing async contract interactions is uncharted territory. Getting
this right is a major differentiator.

### 3. No Gas Fees + Instant Finality

Transaction lifecycle is Pending -> Committed. No confirmation waiting,
no gas estimation dance, no "is it included?" anxiety. This means:

- `groundhog deploy` can be **synchronous by default** — deploy, wait
  for commit, return receipt
- No need for the complex gas estimation UX that plagues EVM tooling
- Storage cost model exists but is simpler (tokens, not gas auctions)

### 4. Smaller Ecosystem = Opinionated Defaults

Foundry supports hundreds of chains, thousands of tokens, complex DeFi
composability. Groundhog can be more opinionated: one chain, one config
format, one key type (Ed25519), one transaction shape.

---

## Roadmap

### Phase 1: Close the Loop (Now -> 6 weeks)

*Unblock everything else by completing deploy -> call -> observe.*

**CLI:**

1. **`groundhog deploy`** — Take compiled WASM, build
   `SignedTransaction` (the `tx-builder` crate has the blueprint),
   submit via `groundhog-json-rpc-client`, poll `getTransactionStatus`
   until Committed, print receipt.

2. **`groundhog call`** — Build invocation transaction, submit,
   display result. Support `--dry-run` from day one (simulate locally
   via SDK's `TestEnvironment`).

3. **`groundhog keys generate`** — Ed25519 keypair generation, store
   in `~/.config/groundhog/keys/`. Derive addresses using the
   `pubkey_account_tag` pattern already in `tx-builder`.

4. **`groundhog.toml`** — Project-level config. Start minimal:

   ```toml
   [profile.default]
   rpc_url = "http://localhost:9545"

   [profile.testnet]
   rpc_url = "https://testnet.groundhog.network"
   ```

5. **`--json` flag** on all commands — programmatic output from day
   one.

**RPC Gateway:**

- Get `groundhog_sendRawTransaction` and
  `groundhog_getTransactionReceipt` working end-to-end.
- `groundhog_getTransactionStatus` for polling.

**Dependencies:** CLI adds `groundhog-json-rpc-client` (the auto-generated
jsonrpsee client) and `ed25519-compact` for keys.

**Why first:** Without deploy+call, nothing else matters. The
`tx-builder` crate proves the transaction-building pattern works.

**Early-partner note:** The private crate registry means partners need
`groundhog login` before they can even `cargo build`. Error messages
when creds are missing or expired should be exceptionally clear — this
is their first impression.

### Phase 2: The Local Node (6-12 weeks)

*This is THE feature. Everything after this builds on it.*

**`groundhog node`** (or `groundhog dev`):

The RPC gateway is already a jsonrpsee server. The local dev node
should be a **lightweight mode** of the same service, with an
in-memory backend instead of real hogd/chuckd connections:

- Same RPC interface — all tooling works identically against local
  or remote
- Pre-funded accounts (deterministic from seed, like Anvil)
- Instant "mining" — transactions commit immediately
- `--state state.json` for persistence between sessions
- `--reset` for clean slate
- Auto-detection in CLI: if `rpc_url` is localhost and nothing is
  listening, offer to start the node

**Implementation approach:** Add a `DevBackend` to the gateway that
implements the same trait as the real hogd/scheduler/indexer backends,
but backed by the SDK's native executor. This is the key architectural
insight from Foundry: **Anvil and forge test share the same EVM core.**
The local node and the test environment must share the same native
executor.

**Groundhog-unique features the local node enables:**

- **Delta inspector** — `groundhog node --show-deltas` logs storage
  deltas for every transaction
- **Snapshot browser** — expose block snapshots via an RPC method so
  the CLI can inspect historical state
- **Storage cost dashboard** — show real-time storage token consumption

**Architectural decision:**

| Option | Pro | Con |
|--------|-----|-----|
| **A: Embed executor in CLI binary** | Single binary, zero setup | CLI binary gets large, tight coupling |
| **B: CLI spawns gateway binary** | Clean separation | Two binaries, slower startup |
| **C: Gateway has `--dev` mode, CLI auto-starts it** | Same binary for dev and prod gateway | Still two binaries, but CLI orchestrates |

Recommendation: **Option C as stepping stone, Option A as eventual
goal.** Foundry proves single-binary DX wins. But given the gateway
is a separate repo with private registry deps, Option C is pragmatic
for now.

**Why this is the strategic priority:** Anvil is what made Foundry
beat Hardhat. Without a local node, every iteration requires a remote
testnet. The feedback loop goes from milliseconds to seconds. That's
the difference between "fun to build with" and "tolerable."

**Open-source launch note:** Once the registry is public, the local
node means developers can build and test entirely offline with zero
auth. The build-test loop should require no login — only deploy/call
to a real network needs credentials.

### Phase 3: Testing DX (8-16 weeks, overlapping Phase 2)

*Make `groundhog test` the best testing experience in any smart
contract platform.*

**Steal from forge test:**

1. **Verbosity levels** — Adopt `-v` through `-vvvv`:
   - Default: pass/fail count
   - `-v`: test names + status
   - `-vv`: logs + events from contracts
   - `-vvv`: execution traces (frame entry/exit, call chain)
   - `-vvvv`: storage delta details + merge visualization

2. **`--watch` mode** — Use `watchexec` (Foundry does). Re-run on
   file change. `--rerun-failed` for TDD mode.

3. **`--gas-report`** — Storage cost report per test. The SDK already
   has `StorageCostModel` and `get_current_cost_model()`. Surface it.

4. **`--gas-snapshot`** — Commit a `.storage-snapshot` file, CI checks
   for regressions. Foundry's `snapshot.rs` is 655 lines — the concept
   is powerful.

5. **`--fail-fast`** and **`--rerun`** — Cheap wins. Forward to cargo.

6. **Test filtering** — `--match-test`, `--match-contract` (surface
   cargo's filter args with friendlier names).

7. **`--junit`** — CI-friendly output for GitHub Actions summaries.

**SDK-level test improvements (coordinate with SDK team):**

8. **Time manipulation** — `test_env.advance_time(secs)` (advance
   block timestamps).

9. **State inspection** — `test_env.inspect_storage(addr)` — dump
   storage keys/values.

10. **Fuzzing integration** — `proptest` support in contract tests,
    surface via `--fuzz-runs N`.

### Phase 4: Deployment Scripting (12-20 weeks)

*Reproducible, resumable multi-step deployments.*

**`groundhog script`** — Foundry's `forge script` lets you write
deployment logic in Solidity, simulate it, then broadcast. For
Groundhog:

- **Deployment scripts in Rust** — A `#[groundhog_script]` macro
  that provides a `ScriptContext` with `deploy()`, `call()`,
  `grant_storage()` helpers.
- **Dry-run mode** (default) — simulate against local node or forked
  state.
- **`--broadcast`** — actually submit transactions.
- **Resume on failure** — persist deployment state to disk.
- **Multi-contract orchestration** — deploy A, get address, pass to
  B's initialize.

**`groundhog verify`** — compare deployed WASM against locally compiled
WASM.

### Phase 5: Advanced DX (16-24+ weeks)

*Power-user features.*

| Feature | Foundry Equivalent | Groundhog Opportunity |
|---------|-------------------|----------------------|
| `groundhog debug <tx>` | `forge test --debug` | Step through frames, inspect storage deltas at each step. Deterministic model makes this cleaner than EVM debugging. |
| `groundhog trace <tx>` | `-vvvv` traces | Human-readable execution trace: frame tree + events + storage changes. |
| `groundhog inspect <addr>` | `forge inspect` | ABI (endpoint IDs), storage layout, contract metadata. |
| `groundhog doc` | `forge doc` | Generate docs from `///` comments on contract endpoints. |
| `groundhog repl` | `chisel` | Interactive contract interaction — deploy + call in a REPL loop against local node. |
| `groundhog clone` | `forge clone` | Clone a deployed contract's WASM + ABI from the network. |
| `groundhog fmt` | `forge fmt` | Surface `rustfmt` with contract-aware config. |
| `groundhog update` | `foundryup` | Self-update mechanism. |
| `groundhog coverage` | `forge coverage` | Code coverage for contract tests. |

---

## Patterns to Adopt Now (Cheap, High-Impact)

These can be done incrementally alongside Phase 1 work.

### 1. Actionable Error Messages

Every `CliError` variant should include a suggestion:

```
error: not a groundhog project (no Cargo.toml with groundhog registry)

  hint: run `groundhog new my-project` to create one,
        or check you're in the right directory
```

### 2. Config Profiles via figment

Foundry's `foundry-config` uses figment for layered config. Use the
same for `groundhog.toml`:

```
GROUNDHOG_PROFILE=testnet groundhog deploy
```

### 3. `--json` Output Mode

Every command supports `--json` for programmatic consumption. The
`Print` struct is the right place — add a `json` mode alongside
`quiet`.

### 4. Progress Indicators

WASM compilation is slow. Show a spinner or progress bar. Foundry
uses `indicatif`.

### 5. `groundhog init` as Guided Setup

Currently stubbed. This is the right place for a first-run wizard:
check toolchain -> prompt for registry creds -> verify login ->
scaffold first project -> run build -> run test. One command from
zero to working. Especially valuable for early partners.

---

## Top 5 Priorities

1. **Complete deploy/call loop** (Phase 1) — everything else is
   blocked on this.
2. **`groundhog node` / local dev** (Phase 2) — the single most
   important DX feature.
3. **`--json` + `--watch` + verbosity levels** — cheap wins that
   compound.
4. **`groundhog.toml` with profiles** — foundational config every
   later feature needs.
5. **Storage cost reporting** — leverage what the SDK already tracks.

The local dev node is the strategic bet. It's what separates "a build
tool" from "a development platform."
