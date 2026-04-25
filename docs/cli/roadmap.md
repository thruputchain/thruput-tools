# Groundhog CLI Roadmap

Execution roadmap for the Groundhog CLI as of 2026-04-24.

`docs/foundry.md` is the benchmark/reference document. This file is the
working plan we should actually execute against.
`TODO.md` is a lightweight session tracker only. If `TODO.md` and this
file ever disagree, this file wins.

## Product Goal

Make Groundhog feel fast and obvious for the core contract workflow:

`new -> build -> test -> deploy -> call -> inspect result`

Foundry is the bar for terminal-first DX, but not the product spec.
Groundhog should copy the speed and ergonomics, then lean hard into the
parts that are unique to its model: snapshot isolation, delta merges,
instant finality, and a local node backed by the same native executor as
tests.

## Current Reality

What exists today:

- `groundhog new`
- `groundhog build`
- `groundhog test`
- `groundhog deploy`
- `groundhog call`
- `groundhog node` (feature-gated behind `dev-node`)
- `groundhog init` (interactive first-run wizard)
- `groundhog status` (non-interactive diagnostic)
- `groundhog keys`
- `groundhog login` / `logout`
- shell completions
- `groundhog.toml` with profiles and per-profile contract aliases
- shared check layer (`checks.rs`) with findings model
- clean lib/bin split, central `Print`, XDG config, test coverage for
  current commands

What that means:

- The local build/test loop exists.
- The network interaction loop now exists.
- A local dev node backed by the same native executor as SDK tests exists.
- The first-run experience is solid (`init` wizard + `status`).
- The next work should focus on polishing invocation UX and testing DX
  rather than unblocking the basic transaction loop.

Implementation status:

- Phase 1 is effectively complete: deploy/call work, `--json` works, and
  contract-spec-aware invocation is in place through embedded contract specs
  plus local metadata. The CLI now emits `.spec.json` as an inspection/export
  artifact, not as runtime input.
- Embedded specs now drive first-pass typed invocation too: scalars,
  structs, enums, unions, options, tuples, vectors, arrays, and byte
  blobs all round-trip without relying on the old JSON sidecar shape.
- `groundhog call --dry-run` now exists as a signed, RPC-backed first
  pass: it resolves selectors, encodes calldata, fetches tip-derived
  expiry, signs the transaction, and returns the prepared invocation
  without broadcasting it.
- `groundhog deploy --dry-run` resolves the WASM artifact, builds and
  signs the deploy transaction, computes the predicted contract address,
  and returns all of that without broadcasting.
- Remaining Phase 1 follow-up: cache repeated embedded-spec parsing,
  tighten union/structured CLI ergonomics and diagnostics, and keep
  proving out the spec-driven path so the remaining local-source fallback
  can eventually be removed.
- Phase 2 is complete for the current local-dev scope: `groundhog node`
  ships, shares execution semantics with the SDK/native executor, retains
  transaction detail, exposes events, and supports latest-state storage
  reads through `groundhog_dev_getStorageAt`.
- The current local-node backend is prototype infrastructure, not the
  durable backend contract. The next backend direction is a hogd-side
  embedded execution library consumed from Rust, documented in
  `docs/cli/specs/2026-04-24-embedded-dev-node-backend.md` and
  `docs/cli/plans/2026-04-24-embedded-dev-node-backend.md`.
- Still intentionally deferred: a no-key-required call dry-run / inspect
  split, richer storage-cost reporting, and `groundhog inspect`.

## Execution Tracker

This is the canonical task tracker for the repo.

Maintenance rules:

- Update this section when work lands, scope changes, or sequencing
  changes.
- Keep checklist items concrete and implementation-oriented.
- Use `TODO.md` only for the active working batch, not for long-term
  backlog management.
- If a task matters across sessions, it belongs here.

### Current Surface

- [x] `groundhog new`
- [x] `groundhog build`
- [x] `groundhog test`
- [x] `groundhog deploy`
- [x] `groundhog call`
- [x] `groundhog node` (feature-gated)
- [x] `groundhog init`
- [x] `groundhog status`
- [x] `groundhog keys`
- [x] `groundhog login` / `logout`
- [x] shell completions
- [x] `--quiet`, `--json`, `--profile`
- [x] `groundhog.toml` with profiles and contract aliases

### Phase 0

- [x] Actionable error hints for common local failures
- [x] `groundhog init` preflight
- [x] Project profiles and `--json` output
- [x] Key management (`generate` / `ls` / `use` / `rm`)
- [x] Contract alias parsing and persistence
- [x] Named error variants
- [x] Zeroize key material, separate public key storage
- [x] Config split (`dirs` / `global` / `project`)
- [x] `run_cargo` helper, `toml_edit` writes, address validation
- [x] Expand `groundhog init` into first-run wizard
- [x] `init`: key generation / default key selection
- [x] `init`: inline login handoff when credentials are missing
- [x] `init`: optional scaffold offer for new users
- [x] `groundhog status` command with project/profile validation
- [x] Planning docs cleanup complete

### Phase 1

- [x] Minimal project config: active profile selection, `rpc_url`,
  default key name, per-profile contract aliases / addresses, and
  environment override
- [x] Wire contract spec / IDL generation into the CLI workflow
- [x] Clarify the contract spec / IDL path needed for `call`
- [x] Signing / tx-building: depend on a shared signing library
  extracted from `groundhog-rpc/crates/tx-builder`.
  Current direction: extract a dedicated `groundhog-signing` crate,
  keep `sdk_rs` separate, and consolidate CLI + RPC gateway into one
  off-chain tools workspace. See `docs/signing-reorg-plan.md` and
  `docs/signing-crate-api.md`.
  Includes: converge on one ed25519 crate, preferably aligned with the
  wider Groundhog Rust stack, replace hand-rolled address derivation
  with canonical `PubkeyAccountPreimage` XDR, and bridge `keys.rs` key
  loading to the shared signing library.
- [x] Command result envelope for consistent JSON shape
- [x] Implement `groundhog deploy`
- [x] Implement `groundhog call`
- [x] Add real `--json` payloads for `deploy` and `call`
- [x] Wire deploy to persist/update alias in `groundhog.toml`
- [x] Start deploy/call integration once the RPC client path is ready

### Phase 2

- [x] Local dev node via RPC/native executor backend
- [x] Ensure local node and test environment share execution
- [x] Local-node observability (transaction detail, events, latest-state
  storage reads)
- [ ] Extract current dev-node backend behind a swappable backend trait
- [ ] Replace prototype `test_ffi` backend with supported embedded hogd
  execution library when available

### Phase 3

- [ ] `groundhog test --integration`
- [x] `groundhog test --fail-fast`
- [x] `groundhog test --match-test`
- [x] `groundhog test --show-output`
- [x] `groundhog test --no-capture`
- [x] `groundhog test --test-threads`
- [x] `groundhog test --skip`
- [x] `groundhog test --rerun`
- [x] `groundhog test --watch`
- [ ] Better test output controls / machine-readable output
- [ ] Storage cost reporting
- [ ] `groundhog inspect`

## Strategy

Order of operations:

1. Remove friction in the current CLI.
2. Complete the transaction loop.
3. Add a local node using the same execution core as tests.
4. Improve testing DX on top of that foundation.

This is intentionally narrower than the Foundry analysis. We should not
start `debug`, `coverage`, `repl`, `doc`, or scripting work until the
core loop and local dev loop are solid.

## Phase 0: Tighten The Existing Surface

Goal: make the current CLI trustworthy and easier to adopt.

### Deliverables

1. **Actionable error messages**
   - Add hints for common failures:
   - missing `cargo`
   - missing `wasm-opt`
   - not in a Groundhog project
   - missing or expired login credentials

2. **`groundhog init` as first-run setup**
   - Check required tools.
   - Ensure cargo registry config exists.
   - Explain whether login is needed.
   - Guide the user through key generation / default key selection.
   - Support inline login handoff.
   - Optionally guide the user into `new` and a scaffold/build/test
     smoke path.

3. **Planning/docs cleanup**
   - Keep `docs/foundry.md` as benchmark/reference.
   - Keep this file short and current.
   - Keep `TODO.md` as a lightweight working tracker that points back to
     this file instead of duplicating it.

### Exit Criteria

- A new partner can understand what is missing from any failure without
  reading source or asking for help.
- `groundhog init` is useful even before deploy/call exist.
- Repo planning docs no longer contradict the codebase.

## Phase 1: Close The Transaction Loop

Goal: ship the minimum complete network workflow.

### Deliverables

1. **Minimal project config**
   - Add `groundhog.toml`.
   - Start with:
   - active profile selection
   - `rpc_url`
   - default key name
   - per-profile contract aliases / deployed addresses
   - environment override for profile selection
   - Do not overbuild config layering on day one.

2. **Contract spec generation**
   - Generate a machine-readable contract spec from SDK macros.
   - Make this the source of truth for:
   - `groundhog call`
   - future `groundhog inspect`
   - future client code generation
   - Keep scope tight: enough for CLI invocation and inspection first.

3. **Key management**
   - Implement `groundhog keys generate`
   - Implement `groundhog keys ls`
   - Implement `groundhog keys use`
   - Implement `groundhog keys rm`

4. **Deploy**
   - Resolve WASM path automatically unless `--wasm` is given.
   - Build a signed deploy transaction.
   - Submit via RPC client.
   - Poll until committed or failed.
   - Print receipt and deployed address.
   - Offer a way to persist/update the deployed address in
     `groundhog.toml`.

5. **Call**
   - Build and submit invocation transactions.
   - Resolve contract aliases from `groundhog.toml`.
   - Prefer contract spec-driven invocation over hardcoded endpoint IDs.
   - First pass of spec-driven typed invocation should cover custom
     structs/enums/unions plus composite containers, not just scalars.
   - Byte vectors / byte arrays must follow `xdrs` opaque encodings
     exactly, because dry-run and live call should share one invocation layer.
   - Print returned value, status, and relevant receipt data.
   - After the first pass, cache spec loads and tighten CLI ergonomics
     before adding `--dry-run`.
   - Add a basic `--dry-run` mode if the SDK support is easy to wire.
   - If dry-run adds too much drag, ship live call first.

6. **Structured output where it matters**
   - Add `--json` first for:
   - `keys`
   - `deploy`
   - `call`
   - Defer full `--json` support for `build` and `test` until the cargo
     passthrough story is cleaner.

### Exit Criteria

- A user can deploy and call a contract without leaving the CLI.
- A script can consume `deploy` and `call` output via `--json`.
- `groundhog.toml` is good enough for local and testnet use.

## Phase 2: Local Dev Node

Goal: eliminate dependency on a remote network for the main inner loop.

### Deliverables

1. **Dev backend in the RPC gateway**
   - Shipped as a standalone `dev-node` crate implementing the same RPC
     interface as the real service.
   - Backed by the SDK/native executor.

2. **Local node command**
   - Shipped as `groundhog node` behind the `dev-node` feature.
   - Keep the UX aligned with the standalone dev node rather than reviving
     an older gateway `--dev` plan.

3. **Core local-node features**
   - deterministic funded accounts
   - instant commit
   - resettable in-memory state
   - optional persisted state file

4. **Groundhog-specific observability**
   - delta logging
   - snapshot browsing / latest-state storage reads
   - event queries
   - storage cost visibility remains part of Phase 3 reporting work

### Exit Criteria

- `deploy` and `call` work unchanged against local and remote RPC URLs.
- A developer can build, test, deploy, and iterate offline.
- The node and the test environment share the same execution semantics.

## Phase 3: Testing DX

Goal: make `groundhog test` fast, inspectable, and CI-friendly.

### Deliverables

1. **Cheap CLI wins**
   - Shipped: `--match-test`, `--fail-fast`, `--show-output`,
     `--no-capture`, `--test-threads`, `--skip`, `--rerun`,
     `--watch`
   - Remaining: `--integration`
   - When the local node exists, `--integration` should orchestrate:
     start local node, deploy contract(s), run integration tests, and
     clean up local state unless persistence is requested

2. **Output controls**
   - verbosity levels
   - better event/log display
   - optional machine-readable test output

3. **Groundhog-native reporting**
   - storage cost report
   - storage snapshot baseline checks
   - trace/delta visibility where the SDK already exposes enough data

### Exit Criteria

- The common TDD loop is one command.
- CI can consume test output cleanly.
- Groundhog-specific runtime behavior is visible during failures.

## Not In Scope Yet

Do not treat these as near-term roadmap items:

- `groundhog script`
- `groundhog debug`
- `groundhog trace`
- `groundhog coverage`
- `groundhog doc`
- `groundhog repl`
- `groundhog clone`
- `groundhog update`
- multi-contract workspace support
- broad Foundry parity work for its own sake

These may matter later, but they are downstream of the core execution
loop and local node.

`groundhog inspect` is intentionally not in the near-term command set,
but the contract-spec work in Phase 1 should preserve a clean path to it.

## Immediate Next Steps

The next concrete batch should continue Phase 3 testing DX:

1. Add `groundhog test --integration` on top of the local dev node:
   - start a local node
   - deploy the target contract
   - run integration tests against its RPC URL
   - clean up unless persistence is requested
2. Add better test output controls:
   - human verbosity levels
   - optional machine-readable output
   - event / delta snippets on failures
3. Define the first storage-cost reporting surface:
   - CLI output shape
   - JSON fields
   - relationship to existing transaction detail and storage deltas

## Decision Rules

Use these rules when choosing what to build next:

- Prefer shortening the feedback loop over adding commands.
- Prefer Groundhog-native strengths over EVM mimicry.
- Prefer a smaller complete workflow over a larger stubbed surface.
- Prefer features with clear exit criteria over aspirational DX lists.
