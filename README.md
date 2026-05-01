# Groundhog Developer Tools

Off-chain developer tooling for the Groundhog smart contract
platform. Monorepo containing the CLI, RPC gateway, reusable RPC
server layer, transaction signing library, and typed client.

## Crates

| Crate | Description |
|-------|-------------|
| [`cli`](crates/cli/) | `groundhog` CLI for the contract dev workflow |
| [`gateway`](crates/gateway/) | JSON-RPC 2.0 gateway server |
| [`signing`](crates/signing/) | Off-chain transaction signing and construction |
| [`client`](crates/client/) | Typed JSON-RPC client |
| [`api-types`](crates/api-types/) | Shared RPC types |
| [`rpc-api`](crates/rpc-api/) | Shared jsonrpsee trait definitions |
| [`rpc-server`](crates/rpc-server/) | Shared server-side JSON-RPC implementation and validation |
| [`x23-transport`](crates/x23-transport/) | Shared x23 connection-pool helpers (used by gateway and node-supervisor) |
| [`dev-node`](crates/dev-node/) | Experimental local JSON-RPC dev node (`native_exec` prototype backend) |
| [`node-supervisor`](crates/node-supervisor/) | `groundhog node` facade: supervises hogd-dev and serves JSON-RPC via `rpc-server` (durable local-node path; `chainTip` wired to hogd-dev scheduler, submit/read pending x23 adapters) |
| [`e2e-tests`](crates/e2e-tests/) | Process-level JSON-RPC smoke tests |
| [`bench`](crates/bench/) | Benchmark harnesses |

## Quick Start

```bash
cargo build -p groundhog-cli
cargo run -p groundhog-cli -- init
cargo run -p groundhog-cli -- new my-counter
```

For the experimental local dev node:

```bash
cargo run -p groundhog-cli --features dev-node -- node
```

Or install the CLI with experimental local dev-node support:

```bash
cargo install --path crates/cli --features dev-node
```

Typical local contract loop:

```bash
groundhog new my-counter && cd my-counter
groundhog build
groundhog node  # in another terminal
groundhog deploy
groundhog call my-counter get
```

See [`crates/cli/README.md`](crates/cli/README.md) for full CLI
documentation.

## Development

```bash
cargo build                             # build all crates
cargo test --workspace                  # run all tests
cargo clippy --workspace --all-targets -- -D warnings # lint
cargo fmt --check                       # format check
```

## Documentation

- [CLI documentation](crates/cli/README.md)
- [CLI roadmap](docs/cli/roadmap.md)
- [Hogd-dev hybrid dev-node design](docs/cli/2026-04-25-hogd-dev-node-hybrid.md)
- [Local node hogd-dev supervisor plan](docs/cli/plans/2026-04-29-local-node-hogd-dev-supervisor.md)
- [Hogd-dev binary bootstrap plan](docs/cli/plans/2026-04-30-hogd-dev-binary-bootstrap.md)
- [Hogd-dev process contract proposal](docs/cli/specs/2026-04-29-hogd-dev-process-contract.md)
- [Node-supervisor x23 client boundary](docs/cli/specs/2026-04-29-node-supervisor-x23-client-boundary.md)
- [Gateway system design](docs/gateway/system-design.md)
- [Gateway API sketch](docs/gateway/api-sketch.md)
- [Gateway roadmap](docs/gateway/roadmap.md)

## License

Apache-2.0
