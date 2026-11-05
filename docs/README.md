# Developer documentation

Start with the [repository README](../README.md) to install the tools and run a
contract locally. Read the [architecture guide](architecture.md) for component
relationships, repository boundaries, and a source map.

## References

- [CLI commands](../crates/cli/README.md)
- [JSON-RPC methods, types, and errors](../crates/json-rpc/open-rpc-spec/snapshots/thruput-spec.md)
- [Contract client](../crates/client/README.md)
- [Application testkit](../crates/testkit/README.md)

## Operating guides

- [Connect a gateway to a local node](gateway/runtime-manifest.md)
- [Run and compare benchmarks](gateway/bench-scenarios.md)
- [Measure transaction-status misses](gateway/lookup-bench.md)
- [Check gateway health and metrics](gateway/observability-smoke-test.md)
- [Inspect firehose records](firehose/inspecting-records.md)

## Maintenance

- [Publish a release](releases.md)
- [Update the CLI's bundled documentation](bundled-documentation.md)

Keep setup in the repository README, API behavior beside the code, and regression
expectations in tests. Use this directory for system context and operating
procedures. Link to source for types, defaults, and algorithms. Remove completed
plans and release evidence; Git history retains them.
