# Groundhog Docs

This directory has two kinds of documents:

- **Living docs** describe the current user, operator, and implementation
  contract.
- **Archive docs** preserve older strategy notes, shipped implementation plans,
  and competitive analysis. They are useful context, but the current code and
  living docs win when they disagree.

## Start Here

- [CLI roadmap](cli/roadmap.md) - active CLI execution tracker.
- [Runtime manifest quickstart](gateway/runtime-manifest.md) - current
  local-node, gateway, and benchmark handoff contract.
- [Gateway benchmark bootstrap plan](gateway/benchmark-bootstrap-plan.md) -
  active benchmark/bootstrap roadmap, including bootstrap-manifest input.
- [Groundhog simulation](sim/) - current `hogsim` docs for stateful
  end-to-end chain traffic, soak tests, target orchestration, and result
  artifacts.
- [Benchmark scenario library](gateway/bench-scenarios.md) - repeatable local
  benchmark scenarios.
- [Gateway roadmap](gateway/roadmap.md) - high-level gateway product roadmap.

## Reference

- [Gateway system design](gateway/system-design.md) - gateway architecture
  background and current backend topology notes.
- [Gateway open questions](gateway/open-questions.md) - remaining cross-team
  questions and resolved decisions.
- [Hog-devnode process contract](cli/specs/2026-04-29-hogd-dev-process-contract.md)
  - current `groundhog node` child-process contract.
- [CLI specs](cli/specs/) - current contracts, deferred decisions, and active
  options papers. Shipped implementation plans move to the archive unless they
  remain the current operator/user contract.

## Archive

- [CLI archive](archive/cli/) - shipped implementation plans, historical design
  context, and competitive DX analysis.
- [Gateway archive](archive/gateway/) - historical gateway sketches superseded
  by the `json-rpc-api` trait and the generated OpenRPC snapshot.
- [Superpowers archive](archive/superpowers/) - shipped artifact-layout and
  instrumentation plans.
