# Groundhog Developer Tools

Off-chain developer tooling for the Groundhog smart contract
platform. Monorepo containing the CLI, RPC gateway, transaction
signing library, and typed client.

## Crates

| Crate | Description |
|-------|-------------|
| [`cli`](crates/cli/) | `groundhog` CLI for the contract dev workflow |
| [`gateway`](crates/gateway/) | JSON-RPC 2.0 gateway server |
| [`signing`](crates/signing/) | Off-chain transaction signing and construction |
| [`client`](crates/client/) | Typed JSON-RPC client |
| [`api-types`](crates/api-types/) | Shared RPC types |
| [`tx-builder`](crates/tx-builder/) | Thin CLI wrapper for tx construction |

## Quick Start

```bash
cargo build -p groundhog-cli
cargo run -p groundhog-cli -- init
cargo run -p groundhog-cli -- new my-counter
```

See [`crates/cli/README.md`](crates/cli/README.md) for full CLI
documentation.

## Development

```bash
cargo build                             # build all crates
cargo test --workspace                  # run all tests
cargo clippy --workspace -- -D warnings # lint
cargo fmt --check                       # format check
```

## Documentation

- [CLI documentation](crates/cli/README.md)
- [CLI roadmap](docs/cli/roadmap.md)
- [Gateway system design](docs/gateway/system-design.md)
- [Gateway API sketch](docs/gateway/api-sketch.md)
- [Gateway roadmap](docs/gateway/roadmap.md)

## License

Apache-2.0
