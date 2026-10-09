# Groundhog Developer Tools

This repository contains Groundhog's off-chain tools: the `thruput` CLI,
JSON-RPC gateway and clients, signing and RV32 build libraries, hosted faucet,
and local development, simulation, and benchmark harnesses. The node runtime
(`hog-devnode`) lives in the separate `hogd` repository.

- **Writing a contract?** Install the CLI below and follow the local loop.
- **Integrating an application?** See [Clients and API](#clients-and-api).
- **Changing these tools?** Start at [Contributing](#contributing).

## Install

Building from source requires Rust 1.96 or newer. This workspace and generated
contract projects currently depend on packages in the private Groundhog Cargo
registry. Repository access alone is not enough to build them; the commands
below require registry credentials and authentication instructions from the
Groundhog team.

Generated contracts pin their own nightly toolchain, which rustup installs on
first use.

Install from the registry:

```sh
export CARGO_REGISTRIES_GROUNDHOG_INDEX="sparse+https://crates.thruput.network/api/v1/crates/"
cargo install thruput-cli --registry groundhog --locked
thruput --version
```

Or, from the root of this checkout:

```sh
cargo install --path crates/cli --locked --force
```

Cargo installs `thruput` under `~/.cargo/bin`; make sure it is on your
`PATH`. `--force` replaces an existing installation.

## Run your first contract locally

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
thruput node
```

Leave it running. In another terminal, enter the same project directory and
deploy and read the counter:

```sh
thruput deploy --fund 10 --profile local
thruput call my-counter get --profile local
```

The node listens on `127.0.0.1:9545` and is ephemeral by default. Use
`thruput node --data-dir .thruput/node` to preserve state across restarts.
`thruput test --integration` manages its own ephemeral node for the project's
integration tests.

For a runtime built in `hogd`, set
`THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode` before starting the node.
See the [toolchain reference](crates/cli/README.md#thruput-toolchain)
for supported hosts, version selection, and local overrides.

For managed runtime installation, wallets, hosted Devnet, container networking,
and the full command reference, see the [CLI guide](crates/cli/README.md).
Use `thruput <command> --help` for the installed version's options.
`thruput docs` serves the bundled developer documentation locally.

## Clients and API

The public API uses JSON-RPC methods named `groundhog_*`. Start with the
[generated OpenRPC specification](crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json)
for methods, types, and errors. Its wire-format version is independent of the
Rust workspace version and is reported by `groundhog_version` as `wireFormat`.

Wire format 11 encodes every 64-bit-or-wider integer as a canonical decimal string,
including small values such as `"0"`. This applies to requests and responses;
JSON numbers are rejected for these fields. Smaller bounded integers remain JSON
numbers. The TypeScript client exposes wide integers as `bigint`. CLI JSON reports
follow the same precision policy, with independently versioned command schemas.

- [Rust client](crates/client/): contract invocation and supplied-spec codecs.
- [Typed Rust RPC client](crates/json-rpc/json-rpc-client/): the JSON-RPC surface.
- [TypeScript client](https://github.com/groundhogchain/client-ts):
  `@groundhogchain/client`, also used by generated contract bindings.
- [TypeScript protocol bindings](https://github.com/groundhogchain/protocol-ts):
  XDR and contract-spec primitives.

The gateway and local node share the same public server implementation.
Internal x23 services and runtime manifests connect the tooling to `hogd`;
application clients use JSON-RPC. See the
[runtime manifest quickstart](docs/gateway/runtime-manifest.md) when working
on gateway or benchmark integration.

## Contributing

From the repository root, rustup uses [rust-toolchain.toml](rust-toolchain.toml).
Configure private registry access, then build and run the core checks:

```sh
cargo build --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Run the CLI without installing it with
`cargo run -p thruput-cli -- --help`. `make help` lists convenience targets.

For the complete local contract loop against a real runtime:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode scripts/smoke-local-dx.sh
```

Process-level smokes need registry access and a compatible runtime; the ordinary
workspace test command does not run ignored live-node tests.
[CI](.github/workflows/ci.yml) also checks generated projects and TypeScript
bindings.

The counter-app browser smoke generates a fresh project and verifies wallet
funding, execution, failure feedback, reloads, state across frontend restarts,
explicit redeployment, node resets, and cleanup:

```sh
npm ci --prefix scripts/scaffold-app
npx --prefix scripts/scaffold-app playwright install chromium
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode npm test --prefix scripts/scaffold-app
```

It requires Node.js 22.12+, Groundhog Cargo and npm registry access, and a compatible
runtime. `CHROMIUM_EXECUTABLE` can select an installed Chromium. The harness owns
its node and uses isolated CLI configuration and dynamically allocated ports;
the generated app connects to the node without managing it.

Crates are grouped by subsystem: `crates/json-rpc` (public API and gateway),
`crates/x23` (hogd adapters), and `crates/rv32` (artifact format and lowering).
Other crates, including `cli`, `client`, `node-supervisor`, and `testkit`, live
directly under `crates/`. See the [crate layout](crates/README.md);
[Cargo.toml](Cargo.toml) lists every workspace crate.

Use feature branches and pull requests. Follow existing Rust conventions:
typed `thiserror` errors, no `.unwrap()` outside tests, and documented public
behavior. Include tests for changed behavior and describe what you verified.

- [Workspace architecture](docs/architecture.md): component boundaries and source map.
- [Documentation index](docs/README.md): design rationale, operating guides, and proposals.
- [Simulator guide](docs/sim/README.md): stateful traffic and soak tests.
- [Release instructions](docs/releases.md): publishing and binary artifacts.
- [Bundled documentation maintenance](docs/bundled-documentation.md): updating
  the developer guide shipped with the CLI.

## License

[Apache-2.0](LICENSE)
