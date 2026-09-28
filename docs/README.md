# Groundhog Docs

This directory has two kinds of documents:

- **Living docs** describe the current user, operator, and implementation
  contract.
- **Archive docs** preserve older strategy notes, shipped implementation plans,
  and competitive analysis. They are useful context, but the current code and
  living docs win when they disagree.

Every committed document under `docs/` is listed here. When you add one, add
it to the matching section; when a plan ships, move it to the archive or mark
its status line so this index stays honest.

## Start Here

- [CLI roadmap](cli/roadmap.md) - active CLI execution tracker.
- [Runtime manifest quickstart](gateway/runtime-manifest.md) - current
  local-node, gateway, and benchmark handoff contract.
- [Gateway benchmark bootstrap plan](gateway/benchmark-bootstrap-plan.md) -
  active benchmark/bootstrap roadmap, including bootstrap-manifest input.
- [Groundhog simulation](sim/) - current `hogsim` docs for stateful
  end-to-end chain traffic, soak tests, target orchestration, and result
  artifacts.
- [Scheduler lookup miss benchmark](gateway/lookup-bench.md) - targeted admission,
  queue, and backend-call diagnostics using never-submitted hashes.
- [Benchmark scenario library](gateway/bench-scenarios.md) - repeatable local
  benchmark scenarios.
- [Gateway roadmap](gateway/roadmap.md) - high-level gateway product roadmap.

## Gateway

- [Gateway system design](gateway/system-design.md) - gateway architecture
  background and current backend topology notes.
- [Gateway open questions](gateway/open-questions.md) - remaining cross-team
  questions and resolved decisions.
- [Observability, health, and metrics plan](gateway/observability-health-metrics-plan.md) -
  the `/health`, `/ready`, and `/metrics` contract the gateway serves.
- [Observability smoke test](gateway/observability-smoke-test.md) - manual
  checks for the observability listener against a running gateway.
- [Transaction ingress and proposer-local races](gateway/tx-ingress-and-proposer-race.md) -
  why admission is not commitment, and how the gateway reports each stage.
- [Runtime manifest example](gateway/examples/runtime-manifest.hogd-dev.example.json)
  and [bootstrap manifest example](gateway/examples/bootstrap-manifest.local-dev.example.json) -
  illustrative v2 manifest shapes. Not runnable: ports, addresses, and hashes
  are runtime state.
- [Network info and airdrop RPC design](gateway/specs/2026-08-22-network-info-and-airdrop-rpc-design.md) -
  shipped; retained for the `groundhog_networkInfo` / `groundhog_requestAirdrop`
  design rationale.

## CLI

- [Hog-devnode process contract](cli/specs/2026-04-29-hogd-dev-process-contract.md)
  - current `groundhog node` child-process contract.
- [Transaction detail options](cli/specs/2026-05-07-transaction-detail-options.md) -
  receipt response shape and deferred deep-detail decisions.
- [Remote devnet and faucet design](cli/specs/2026-06-25-remote-devnet-faucet-design.md) -
  hosted network UX, faucet funding, and wallet-create flow.
- [Global and project config brief](cli/specs/2026-07-02-global-and-project-config-brief.md)
  and [config management design](cli/specs/2026-07-23-config-management-design.md) -
  shipped profile model; the design is the reference for merge and precedence
  rules.
- [Faucet hardening workstream](cli/specs/2026-07-08-faucet-hardening-workstream.md) -
  faucet policy, idempotency, and CLI retry semantics.
- [Transfer and transaction inspection](cli/specs/2026-07-23-transfer-and-transaction-inspection-design.md)
  and [event and token transfer inspection](cli/specs/2026-07-27-event-and-token-transfer-inspection-design.md) -
  shipped `transfer`, `tx`, `events`, and `token` command designs.
- [Status redesign plan](cli/specs/2026-07-28-status-redesign-plan.md) -
  superseded; `groundhog status` shipped a different JSON shape (see the CLI
  README).
- [TypeScript binding codegen upgrade](cli/specs/2026-08-14-typescript-binding-codegen-upgrade.md) -
  `groundhog codegen ts` output contract.
- [WASM removal design](cli/specs/2026-08-17-wasm-removal-design.md) - shipped;
  the audit that made RV32 the only contract target.
- [Managed devnode toolchain](cli/specs/2026-09-11-managed-devnode-toolchain.md) -
  `groundhog toolchain` contract and catalog qualification evidence.

## Proposals

- [Dev-node transaction submission batching](specs/2026-09-04-dev-node-submit-batching.md) -
  implemented; design and acceptance criteria for batching embedded
  local-node submissions.
- [CLI TUI MVP](specs/2026-09-21-cli-tui-mvp.md) - draft; not started.
- [Project scaffolding](specs/2026-09-21-project-scaffolding.md) - draft
  for review.

## RV32 Artifact Format

Design notes for `crates/rv32/artifact` and `crates/rv32/lower`. The code is
the current contract; these record why each cut was made.

- [Harvard-split lowering](rv32/2026-06-02-lower-harvard-split-design.md)
- [v4 artifact cut](rv32/2026-06-02-v4-artifact-cut-design.md)
- [v5 initial-memory load base](rv32/2026-07-04-v5-load-base-design.md)

## Testkit

- [Integration-test harness design](testkit/DESIGN.md) - `groundhog-testkit`
  provider boundary, scenario model, and phased plan.

## Archive

- [CLI archive](archive/cli/) - shipped implementation plans, historical design
  context, and competitive DX analysis (Foundry, Solana).
- [Gateway archive](archive/gateway/) - historical gateway sketches superseded
  by the `json-rpc-api` trait and the generated OpenRPC snapshot.
- [Artifact-layout archive](archive/superpowers/) - shipped `groundhog build`
  artifact-layout and idempotent-instrumentation plans.
