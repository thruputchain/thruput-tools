# Thruput Developer Tools

[Website](https://www.thruput.network/) ·
[Developer docs](https://www.thruput.network/docs/) ·
[Community discussions](https://github.com/thruputchain/thruput-docs/discussions) ·
[Discord: #thruput](https://discord.com/channels/897514728459468821/1564948118162898954)
([join server](https://discord.gg/stellardev))

This repository contains Thruput's off-chain tools: the `thruput` CLI,
JSON-RPC gateway and clients, signing and RV32 build libraries, hosted faucet,
and local development, simulation, and benchmark harnesses. The node runtime
(`hog-devnode`) lives in the separate `hogd` repository.

- **Writing a contract?** Install the CLI below and follow the local loop.
- **Integrating an application?** See [Clients and API](#clients-and-api).
- **Changing these tools?** Start at [Contributing](#contributing).

## Install

Building from source requires Rust 1.96 or newer. External Rust dependencies
come from crates.io; no registry credentials are needed.

Generated contracts pin their own nightly toolchain, which rustup installs on
first use.

Install from crates.io:

```sh
cargo install thruput --locked
thruput --version
```

Or, from the root of this checkout:

```sh
cargo install --path crates/cli --locked --force
```

Cargo installs `thruput` under `~/.cargo/bin`; make sure it is on your
`PATH`. `--force` replaces an existing installation.

## Run your first contract locally

On Ubuntu/Debian, the Linux runtime needs the system jemalloc library:

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends -y libjemalloc2
```

Install the CLI's selected runtime bundle, then create and build a project:

```sh
thruput toolchain install
thruput init
thruput new my-counter --template counter
cd my-counter
thruput status
thruput build
thruput test
```

Start the node from the generated project directory:

```sh
thruput node --ramdb-bytes 1073741824
```

Leave it running. In another terminal, enter the same project directory and
deploy and read the counter:

```sh
thruput deploy --fund 10 --profile local
thruput inspect my-counter --profile local
thruput call my-counter get --profile local
```

The node listens on `127.0.0.1:9545` and is ephemeral by default. Use
`thruput node --data-dir .thruput/node` to preserve state across restarts.
`thruput test --integration` manages its own ephemeral node for the project's
integration tests.
The example above allocates a 1 GiB RAM database. The runtime's default is
15 GiB; on smaller hosts, also pass
`--node-arg "--ramdb-bytes 1073741824"` to `thruput test --integration`.

For a runtime built in `hogd`, set
`THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode` before starting the node.
See the [toolchain reference](crates/cli/README.md#thruput-toolchain)
for supported hosts, version selection, and local overrides.

See the [CLI guide](crates/cli/README.md) for the full command reference, and
`thruput <command> --help` for the installed version's options. `thruput docs`
serves the bundled developer documentation locally.

## Clients and API

The [JSON-RPC reference](crates/json-rpc/open-rpc-spec/snapshots/thruput-spec.md)
lists the public methods, types, and errors. It is generated from the
[OpenRPC specification](crates/json-rpc/open-rpc-spec/snapshots/thruput-spec.json),
which defines the exact schemas and wire encodings. The wire-format version is
independent of the workspace version; `getVersionInfo` reports it as `wireFormat`.

- [Rust client](crates/client/): contract invocation and supplied-spec codecs.
- [Typed Rust RPC client](crates/json-rpc/json-rpc-client/): the JSON-RPC surface.
- [TypeScript client](https://github.com/thruputchain/client-ts):
  `@thruputchain/client`, also used by generated contract bindings.
- [TypeScript protocol bindings](https://github.com/thruputchain/thruput-protocol-ts):
  XDR and contract-spec primitives.

The gateway and the local node share one public server. Applications use
JSON-RPC; the tools reach `hogd` through internal x23 services. For gateway or
benchmark work, see the [runtime manifest quickstart](docs/gateway/runtime-manifest.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution and review expectations,
and [SECURITY.md](SECURITY.md) to report a vulnerability privately.

From the repository root, rustup uses [rust-toolchain.toml](rust-toolchain.toml).
Build and run the core checks:

```sh
cargo build --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Run the CLI without installing it with
`cargo run -p thruput -- --help`. `make help` lists convenience targets.

For the complete local contract loop against a real runtime:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode scripts/smoke-local-dx.sh
```

Process smokes need Python 3, crates.io access, and a compatible runtime. The workspace
test command skips ignored live-node tests. [CI](.github/workflows/ci.yml) also
checks generated projects and TypeScript bindings.

The counter-app browser smoke drives a freshly generated app against a real node:

```sh
npm ci --prefix scripts/scaffold-app
npx --prefix scripts/scaffold-app playwright install chromium
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode npm test --prefix scripts/scaffold-app
```

It needs Node.js 22.12 or newer, public npmjs and crates.io access, and a
compatible runtime. `CHROMIUM_EXECUTABLE` selects an
installed Chromium.

See the [crate layout](crates/README.md) for how crates are grouped.

Use feature branches and pull requests. Use typed `thiserror` errors, no
`.unwrap()` outside tests, and document public behavior. Add tests for changed
behavior, and state what you verified.

- [Workspace architecture](docs/architecture.md): component boundaries and source map.
- [Documentation index](docs/README.md): references and operating guides.
- [Release instructions](docs/releases.md): publishing and binary artifacts.
- [Bundled documentation maintenance](docs/bundled-documentation.md): updating
  the developer guide shipped with the CLI.

## License

Copyright 2026 Stellar Development Foundation. Licensed under the Apache
License, Version 2.0. Each published crate includes the workspace
[LICENSE](LICENSE).
