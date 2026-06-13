# Groundhog CLI Roadmap

Execution roadmap for the Groundhog CLI as of 2026-05-23.

Historical competitive-analysis notes live under `docs/archive/cli/`.
This file is the working plan we should actually execute against.
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
- `groundhog node`
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
- `groundhog storage` now exposes the public `groundhog_getStorageAt`
  latest-state read as a raw address/key inspection command. This keeps
  the CLI split explicit: `call` signs and submits transactions, `storage`
  reads committed storage, and `view` executes the contract under
  sandboxed simulation.
- Remaining Phase 1 follow-up: cache repeated embedded-spec parsing,
  tighten union/structured CLI ergonomics and diagnostics, and keep
  proving out the spec-driven path so the remaining local-source fallback
  can eventually be removed.
- Phase 2 is complete for the current local-dev scope: `groundhog node`
  ships, shares execution semantics with the SDK/native executor, retains
  transaction detail, exposes events, and supports latest-state storage
  reads through `groundhog_getStorageAt`.
- Runtime-manifest producer/consumer work has landed for `groundhog node`,
  `hog-gateway --runtime-manifest`, and `groundhog-bench
  --runtime-manifest`. CLI `dev-N` wallet-address resolution now uses a
  configured runtime manifest (`GROUNDHOG_RUNTIME_MANIFEST` or
  `profile.<name>.runtime_manifest`). The former local-only dev-account RPC
  has been removed from the OpenRPC surface.
- `groundhog test --json` now emits a stable machine-readable report for both
  WASM and integration modes, including explicit `mode` and integration-node
  metadata for `groundhog test --integration --json`.
- The legacy native-exec local-node backend (`crates/dev-node`) was
  retired in PR 5 once hogd-dev proved out the published-SDK scaffold
  loop. The durable backend is a Rust supervisor
  (`groundhog-node-supervisor`) around a hogd-owned `hogd-dev`
  subprocess. Historical design context lives in
  [`../archive/cli/2026-04-25-hogd-dev-node-hybrid.md`](../archive/cli/2026-04-25-hogd-dev-node-hybrid.md).
- The no-key-required call dry-run / inspect split now has a draft design
  in [`specs/2026-05-07-view-rpc-and-receipt-return-data.md`](specs/2026-05-07-view-rpc-and-receipt-return-data.md);
  it runs as a parallel cross-team track to PR 5 (see Phase 2 tracker).
- Still intentionally deferred: richer storage-cost reporting and
  `groundhog inspect`.

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
- [x] `groundhog view`
- [x] `groundhog storage`
- [x] `groundhog node`
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
- [x] Signing / tx-building: shared `groundhog-signing` crate landed at
  `crates/tools/signing/`. Canonical `PubkeyAccountPreimage` XDR address
  derivation, bridged from `keys.rs`. CLI and RPC gateway now share the
  one off-chain tools workspace.
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
- [x] Extract a swappable backend behind plane traits
  (`SubmitPlane`/`ChainPlane`/`ReadPlane` in `crates/json-rpc/json-rpc-server`,
  consumed by `crates/dev/node-supervisor` over `crates/x23/x23-transport`).
- Durable local-node backend: `groundhog-node-supervisor` around a
  hogd-owned `hogd-dev` subprocess. PRs landed in order below.
  - [x] PR 1 — supervisor crate scaffold (#30)
  - [x] PR 2 — hogd-dev process adapter (#31)
  - [x] PR 3 — CLI integration behind experimental flag (#35)
  - [x] PR 4a — first real hogd-dev chain RPC (`groundhog_chainTip`) (#38)
  - [x] PR 4b — submit RPC (`groundhog_sendRawTransaction`) (#40)
  - [x] PR 4c — read RPC (receipts, events, composite status) on the
    Rust supervisor side (#44); end-to-end coverage gated on hogd-dev
    v1 advertising the `indexer_block_explorer` endpoint
  - [x] PR 4d — supervisor block driver: local single-node nomination and
    commit after accepted submits (#47)
  - [x] PR 4e — dev-account bootstrap: hogd-dev wallet/bootstrap profile,
    readiness account metadata, and CLI `dev-N` wallet-address resolution
    (now manifest-backed; the old dev-account RPC has been removed)
  - [x] PR 4f — published SDK/scaffold compatibility: fresh `groundhog new`
    builds and completes deploy/call against a registry SDK crate matching
    the current hogd-dev protocol.

    Verified on 2026-05-07 after publishing `sdk = "0.14.0"`. At that point the fresh
    scaffold resolves `groundhog-xdr = 0.12.0` and `hogexec-internal = 0.12.0`,
    matching the then-current hogd-dev protocol. Each `groundhog-xdr` crate stamps a
    `PROTOCOL_VERSION`:

    | source                                      | xdr                  | protocol            |
    | ------------------------------------------- | -------------------- | ------------------- |
    | sdk 0.11.4 (template default)               | 0.7.11               | `hogp/cff5b0b2fc94` |
    | sdk 0.13.0 (latest published, intended)     | 0.9.3                | `hogp/e4a1ca88f0ca` |
    | sdk 0.13.0 (normal fresh resolution today)  | 0.9.4                | `hogp/850287355957` |
    | sdk_rs HEAD path checkout (0.14.0)          | 0.10.1               | `hogp/500463c1d97f` |
    | sdk 0.14.0 (published, template default)    | 0.12.0               | `hogp/830b7810ee93` |
    | hogd-dev / hog-gateway current protocol    | 0.13.6               | `hogp/020ed23d4e2f` |

    The protocol hash is the first 12 chars of the `protocol/`
    submodule git commit baked into hogd at build time
    (`hogd/version.h::protocol_commit`); `groundhog-xdr` crate releases
    track the same submodule, so the `xdr` column is what an SDK
    consumer pulls, but the binary's hash comes from the C++ side, not
    from any Rust dep version.

    Two historical failure modes were entangled and are now separated:

    1. *Registry float break.* sdk 0.13.0 declares `groundhog-xdr ^0.9.3`,
       but xdr 0.9.4 made `sys::raw_Return` a 3-arg call. A scaffold
       bumped to `sdk = "0.13.0"` resolves to xdr 0.9.4 and fails to
       compile with "this function takes 3 arguments but 2 arguments were
       supplied." Future SDK publishes should pin xdr with `=` to prevent
       this class of break.
    2. *Protocol skew.* Even sdk 0.13.0's xdr 0.9.3 protocol
       (`hogp/e4a1ca88f0ca`) doesn't match the running hogd-dev
       (`hogp/830b7810ee93` at the time; `hogp/020ed23d4e2f` currently).
       A WASM compiled against the wrong
       protocol deploys cleanly but every call reverts with
       `deterministicError {kind: "unknown"}`. That protocol skew is
       resolved for default scaffolds by the `sdk 0.14.0` publish and
       template bump.

    Follow-ups:

    1. The CLI should fail fast at deploy time when the SDK protocol stamped on
       the artifact differs from the protocol the running node advertises in
       readiness, instead of letting calls revert with a generic deterministic
       error.
    2. Optional but useful: a scaffold-time `--sdk-path` or a documented
       `[patch]` recipe so internal contributors can iterate against a sibling
       sdk_rs checkout without waiting on a republish.

  - [x] PR 5 — local-node `getStorageAt` parity through hogd-dev's
    existing `gateway` / `DbGateway1` endpoint, plus `groundhog storage`
    as the CLI surface for raw committed storage reads. Transaction detail
    and dev-control are deferred; see
    [`specs/2026-05-07-transaction-detail-options.md`](specs/2026-05-07-transaction-detail-options.md).
  - [ ] Read DX follow-ups (parallel track to PR 5; cross-team).
    See [`specs/2026-05-07-view-rpc-and-receipt-return-data.md`](specs/2026-05-07-view-rpc-and-receipt-return-data.md).
    Closes the "no-key-required call dry-run / inspect split" deferred item
    below and the "Keep call return-data surfacing as a separate follow-up"
    note in the supervisor PR plan.
    - [ ] Milestone A — receipt return data and abort data on
      `TransactionReceipt`. Requires hogd `TxEventLog` extension and
      indexer migration; Rust mapping waits on protocol-rs publish.
    - [x] Milestone B — `groundhog_view` RPC and `groundhog view` CLI
      command, landed with sandboxed-simulation semantics.

### Phase 3

- [x] `groundhog test --integration`
- [x] `groundhog test --fail-fast`
- [x] `groundhog test --match-test`
- [x] `groundhog test --show-output`
- [x] `groundhog test --no-capture`
- [x] `groundhog test --test-threads`
- [x] `groundhog test --skip`
- [x] `groundhog test --rerun`
- [x] `groundhog test --watch`
- [x] Machine-readable `groundhog test --json` output for WASM and integration
- [ ] Better human test output controls
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
   - Keep historical benchmark/reference notes under `docs/archive/`.
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
   - Retired as the long-term path. The native-exec `dev-node` prototype was
     deleted in PR 5; the durable backend is hogd-dev supervised by
     `groundhog-node-supervisor`. The git history retains the prototype
     source for any future reference.

2. **Local node command**
   - Shipped as `groundhog node` around a hogd-dev subprocess.
   - Keep the UX aligned with the durable local-node path rather than reviving
     an older gateway `--dev` plan.

3. **Core local-node features**
   - deterministic funded accounts
   - instant commit
   - startup clean slate via restart (hogd-dev is ephemeral; `--reset`
     is only meaningful once an optional persisted state file ships)
   - optional persisted state file

4. **Groundhog-specific observability**
   - delta logging
   - snapshot browsing / latest-state storage reads
   - event queries
   - storage cost visibility remains part of Phase 3 reporting work

### Exit Criteria

- `deploy` and `call` work unchanged against local and remote RPC URLs.
- The default local loop works from a fresh scaffold against a published SDK
  crate, not only against a local SDK checkout.
- A developer can build, test, deploy, and iterate offline.
- The node and the test environment share the same execution semantics.

## Phase 3: Testing DX

Goal: make `groundhog test` fast, inspectable, and CI-friendly.

### Deliverables

1. **Cheap CLI wins**
   - Shipped: `--match-test`, `--fail-fast`, `--show-output`,
     `--no-capture`, `--test-threads`, `--skip`, `--rerun`,
     `--watch`, `--integration`
   - `--integration` now orchestrates the v1 single-contract local-node
     path: start ephemeral hogd-dev, deploy the target contract, run
     `tests/integration.rs` with RPC environment variables, then clean up.
   - Remaining follow-ups: `--watch` / `--rerun` support for the integration
     path, persistence-aware integration runs, and multi-contract orchestration.

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

Phase 2's durable-local-node work is complete through PR 5 and the
view-RPC simulation pivot (PR #76, merged 2026-05-15).  The two
historical Rust-track items are both resolved:

- `groundhog_getStorageAt` parity + `groundhog storage` CLI wrapper
  shipped.
- `groundhog node --reset` deferred — `HogdDevConfig` is documented as
  ephemeral (`crates/dev/node-supervisor/src/config.rs:124-128`), so each
  start is already a clean slate.  `--reset` only becomes meaningful
  once an optional persisted-state file ships, and would be one flag
  on top of that work, not a standalone item.

The hogd-dev/control track and the deferred transaction-detail work
(`docs/cli/specs/2026-05-07-transaction-detail-options.md`) remain
parked until a real hogd-owned dev method needs them.

**Runtime manifest / dev-account cleanup:**

- Runtime-manifest readiness and consumer paths are shipped for the local node,
  gateway, and benchmark harness.
- `groundhog-bench --runtime-manifest` now derives valid dev-wallet submit
  workloads from `bootstrapAccounts`.
- `hog-gateway --runtime-manifest` now starts the standalone gateway from
  advertised backend endpoints.
- CLI `dev-N` wallet-address resolution now uses a configured runtime
  manifest, and the dev-account RPC compatibility path has been removed
  (`GroundhogDevApi`, `groundhog_dev_getAccounts`, generated OpenRPC entries,
  client re-export, and supervisor RPC plumbing).

**Cross-team track (driven by hogd / protocol-rs):**

- Milestone B (view RPC): **shipped**.  Hogd top-level view execution,
  protocol-rs bindings, Rust gateway/local-node wiring, CLI, smoke
  coverage, and spec rewrite all landed with sandboxed-simulation
  semantics.  See
  [`specs/2026-05-07-view-rpc-and-receipt-return-data.md`](specs/2026-05-07-view-rpc-and-receipt-return-data.md).
- Milestone A (receipt return-data + abort-data on
  `TransactionReceipt`): **still open**.  Requires hogd `TxEventLog`
  extension and indexer migration; Rust mapping follows protocol-rs
  publish.  The Rust scaffolding for `abort_data` already exists; both
  backends hardcode `return_data: None` until hogd supplies the bytes.

**Phase 3 testing DX, after `--integration` v1:**

1. Add better test output controls:
   - human verbosity levels
   - event / delta snippets on failures
2. Extend `groundhog test --integration`:
   - `--watch` / `--rerun` support for RPC-backed tests
   - optional persistence-aware runs once persisted local-node state exists
   - multi-contract deployment when workspace support is real
3. Define the first storage-cost reporting surface:
   - CLI output shape
   - JSON fields
   - relationship to existing transaction detail and storage deltas
4. Design targeted state-surgery dev controls:
   - storage-token/balance patching
   - storage item patching
   - code/contract replacement or per-contract import if a future
     spooning workflow needs it
   - explicit interaction with transaction detail and storage deltas

## Decision Rules

Use these rules when choosing what to build next:

- Prefer shortening the feedback loop over adding commands.
- Prefer Groundhog-native strengths over EVM mimicry.
- Prefer a smaller complete workflow over a larger stubbed surface.
- Prefer features with clear exit criteria over aspirational DX lists.
