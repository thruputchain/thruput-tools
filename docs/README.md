# Thruput documentation

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
- [Testkit](testkit/DESIGN.md): concurrent application tests and their relationship
  to the single-project integration loop.

## Operating the tools

- [CLI reference](../crates/cli/README.md)
- [Local node, gateway, and runtime manifests](gateway/runtime-manifest.md)
- [Benchmark scenarios and comparisons](gateway/bench-scenarios.md)
- [Transaction-status miss benchmark](gateway/lookup-bench.md)
- [Gateway health and observability checks](gateway/observability-smoke-test.md)
- [Inspecting firehose records](firehose/inspecting-records.md)
- [Releases](releases.md)
- [Bundled documentation maintenance](bundled-documentation.md)

## What belongs here

Code, tests, generated schemas, and executable examples are the source of truth
for implementation. Keep prose that explains why the system has its shape, how
components relate, or how to operate it. Link to the owning source instead of
copying types, defaults, algorithms, or command inventories.

Keep proposals labelled and separate from current behavior. When work ships,
retain only the rationale that will help a later contributor; remove the plan,
completion checklist, and session or release evidence. Git history preserves
those records. An archive directory is not a second documentation lifecycle.
