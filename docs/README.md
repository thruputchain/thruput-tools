# Groundhog documentation

Start with the [workspace architecture](architecture.md) for the system's shape
and a map into the source. The [repository README](../README.md) covers setup
and the first local contract loop.

## Design and rationale

Read the guide for the area you are changing:

- [Gateway](gateway/system-design.md): public RPC, backend boundaries, admission,
  and why the local node shares the server implementation.
- [CLI](cli/design.md): configuration, wallet identity, build artifacts, and
  managed runtime acquisition.
- [RV32](rv32/design.md): the ELF-to-artifact boundary and memory layout choices.
- [Simulation](sim/sim-design.md): scheduled traffic, actor state, target ownership,
  and what reproducibility means for a real node.
- [Testkit](testkit/DESIGN.md): concurrent application tests and their relationship
  to simulation and the single-project integration loop.

## Operating the tools

- [CLI reference](../crates/tools/cli/README.md)
- [Local node, gateway, and runtime manifests](gateway/runtime-manifest.md)
- [Benchmark scenarios and comparisons](gateway/bench-scenarios.md)
- [Transaction-status miss benchmark](gateway/lookup-bench.md)
- [Gateway health and observability checks](gateway/observability-smoke-test.md)
- [Simulator quickstart and results](sim/README.md)
- [Releases](releases.md)
- [Bundled documentation maintenance](bundled-documentation.md)

## Proposals

These describe proposed work, not the current implementation:

- [CLI TUI](specs/2026-09-21-cli-tui-mvp.md)
- [Project scaffolding](specs/2026-09-21-project-scaffolding.md)

## What belongs here

Code, tests, generated schemas, and executable examples are the source of truth
for implementation. Keep prose that explains why the system has its shape, how
components relate, or how to operate it. Link to the owning source instead of
copying types, defaults, algorithms, or command inventories.

Keep proposals labelled and separate from current behavior. When work ships,
retain only the rationale that will help a later contributor; remove the plan,
completion checklist, and session or release evidence. Git history preserves
those records. An archive directory is not a second documentation lifecycle.
