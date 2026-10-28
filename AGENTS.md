# Agent guidance

This repository contains Thruput's off-chain tools. Start with
[README.md](README.md) for setup and [Contributing](README.md#contributing)
for development commands. Use the pinned [toolchain](rust-toolchain.toml).
Verify behavior in the affected source and tests.

## Find the owner

| Area | Start here |
| --- | --- |
| Public RPC methods and wire version | `crates/json-rpc/json-rpc-api/src/lib.rs`; see [RPC guidance](crates/json-rpc/AGENTS.md) |
| RPC payloads, codecs, and errors | `crates/json-rpc/json-rpc-types/src/`; reuse its types and decimal/base58/hex adapters |
| Shared public server | `crates/json-rpc/json-rpc-server/`; gateway and local node supply backends |
| Gateway and local runtime lifecycle | `crates/json-rpc/json-rpc-gateway/`, `crates/node-supervisor/` |
| hogd response mapping and internal transport | `crates/x23/hogd-backend/`, `crates/x23/x23-transport/` |
| Embeddable contract invocation | `crates/client/`; keep CLI configuration and node supervision outside this library |
| CLI dispatch and commands | `crates/cli/src/app.rs`; `crates/thruput/` only packages the same binary |
| CLI output, prompts, and JSON reports | `crates/cli/src/print.rs`, `crates/cli/src/print/`; reuse `Print` |
| CLI error metadata | `crates/cli/src/error.rs`, `crates/cli/tests/error_metadata.rs` |
| Generated projects and bindings | `crates/cli/templates/`, `crates/cli/src/codegen.rs`, `crates/cli/tests/` |
| Signing and key storage | `crates/signing/`, `crates/cli/src/keys.rs` |
| Transaction bounds | `crates/tx-validation/`; chain admission checks belong at submission, not offline builds |
| Runtime acquisition | `crates/toolchain/`; lookup is local, installation is explicit |
| RV32 format and ELF lowering | `crates/rv32/artifact/`, `crates/rv32/lower/` |

[Cargo.toml](Cargo.toml) lists all crates. The runtime lives in `hogd`;
reuse `thruput-xdr` for protocol definitions. See
[architecture](docs/architecture.md) for component relationships and
[docs/README.md](docs/README.md) for focused operating guides.

## Shared constraints

- Preserve unrelated changes. Use feature branches and PRs, never commits
  directly to `main`. Use neutral, task-focused commit and PR text.
- Follow the workspace's typed `thiserror` errors. No `.unwrap()` outside
  tests; reserve `.expect("bug: ...")` for invariants, not untrusted input.
- Keep private signing material out of logs and subprocess arguments. Follow
  the existing key-storage and signing helpers for secret handling.
- Keep setup in README, API behavior beside code, and regression expectations
  in tests. Link to these sources instead of copying implementation details.
  Keep proposals distinct from current behavior; remove completed plans.

## Validation

Start with focused tests for the changed behavior. Before pushing Rust changes,
run the core workspace checks from [CI](.github/workflows/ci.yml):

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- These commands do not cover all CI. Scaffold/codegen changes also need the
  applicable generated-project and TypeScript checks from the workflow.
- For snapshots, follow the owning test's update procedure, inspect the diff,
  then rerun without `UPDATE_SNAPSHOTS` to check the result.
- Process smokes need crates.io access, a compatible real `hog-devnode`, localhost
  access, and writable runtime/cache directories. See README prerequisites and
  the relevant script: `scripts/smoke-local-dx.sh`,
  `scripts/smoke-runtime-manifest-bench.sh`, or `scripts/smoke-testkit-local.sh`.
  Resolve environment blockers rather than bypassing the checks.
- For documentation-only changes, check links, paths, command claims against
  source, and `git diff --check`. Full Rust CI is not normally needed.
- Report checks actually run and any unverified boundaries.

## Optional guidance and maintenance

Start with source and tests. If a question remains, use `thruput skill --help`
or `thruput docs --help`; install or load only what the task needs.
Skill content lives in `thruputchain/agent-skills`, outside this repository and
CLI release artifacts. Verify guidance against current dependencies.

Follow [bundled documentation maintenance](docs/bundled-documentation.md) when
changing the shipped documentation snapshot, and
[release instructions](docs/releases.md) for publishing and version changes.
