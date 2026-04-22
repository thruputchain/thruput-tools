# groundhog

CLI tools for Groundhog smart contract development. Wraps the
Rust/WASM build and test workflow into a single binary.

## Install

```bash
cargo install --path crates/cli
```

To include the local dev node:

```bash
cargo install --path crates/cli --features dev-node
```

For local development without installing:

```bash
cargo run -p groundhog-cli --features dev-node -- node
```

Requires the nightly Rust toolchain (pinned via `rust-toolchain.toml`
in generated projects).

## Quick Start

```bash
# Check your environment
groundhog init

# Authenticate with the Groundhog registry during early access
groundhog login

# Create a new contract project (counter template)
groundhog new my-counter && cd my-counter

# Build the WASM contract
groundhog build

# Run contract's tests against the WASM executor
groundhog test

# Start the local dev node (optional, requires --features dev-node)
groundhog node

# Deploy and call against the active profile
groundhog deploy
groundhog call my-counter get
```

If you installed the CLI without `--features dev-node`, `groundhog node`
is unavailable. `deploy` and `call` work either way against any RPC URL;
the feature gate only affects the embedded local node.

## Using cargo directly

Scaffolded projects enable the `dev` feature by default, so
plain `cargo` builds and tests work for the host target:

| Command | What it does |
|---------|-------------|
| `cargo build` | Build for host (IDE, rust-analyzer) |
| `cargo test` | Run unit tests on host |
| `groundhog build` | Cross-compile to WASM (`--no-default-features --release`) |
| `groundhog test` | Run integration tests against the WASM executor |

The `dev` feature enables `std` for native compilation.
`groundhog build` passes `--no-default-features` to disable it
for WASM. The `.cargo/contract.toml` overlay adds `build-std`
and `panic=immediate-abort` for optimized contract builds.

## Commands

### `groundhog init`

Interactive first-run setup wizard. Checks your toolchain,
generates a signing key, stores credentials, and optionally
scaffolds a starter project. Re-running when already configured
is a no-op.

```bash
groundhog init          # guided setup
groundhog init --json   # delegates to `groundhog status --json`
```

### `groundhog status`

Non-interactive diagnostic. Reports environment, keys,
credentials, and project config. Exits 0 when clean, 1 when
findings exist.

```bash
groundhog status              # human-readable report
groundhog status --json       # structured output for scripts
groundhog status --quiet      # exit code only (for CI)
groundhog status --profile p  # check a specific profile
```

### `groundhog new <name>`

Create a new contract project in a subdirectory.

```bash
groundhog new my-counter                    # counter template (default)
groundhog new my-contract --template stub   # minimal template
```

### `groundhog build`

Compile the contract to WASM using `build-std` for `core`/`alloc`.

```bash
groundhog build                # release build
groundhog build --optimize     # + wasm-opt (requires wasm-opt on PATH)
groundhog build --profile testnet   # resolve profile from groundhog.toml
groundhog build -- --verbose   # pass extra args to cargo
```

Outputs the `.wasm` file path and size on success. When spec generation
is available, also emits a human-readable `*.spec.json` sidecar next to
the built WASM. The embedded WASM section remains the source of truth;
the JSON file is for inspection and external tooling.

### `groundhog test`

Run contract tests against the test executor. Requires
`groundhog login` first to fetch the pre-compiled executor
library. Injects download credentials as environment variables
and targets the host platform so tests run natively.

```bash
groundhog test                           # run all tests
groundhog test -- --test test_counter    # run specific test
groundhog test -- -- test_deposit        # filter by test name
```

> **Note:** Plain `cargo test` also works for unit tests that
> don't need the executor. `groundhog test` is required for
> integration tests that use the FFI test harness.

### `groundhog keys`

Manage Ed25519 signing keys. Keys are stored in
`~/.config/groundhog/keys/` with restricted file permissions.

```bash
groundhog keys generate [name]   # generate a new keypair
groundhog keys ls                # list stored keys
groundhog keys use <name>        # set the default signing key
groundhog keys rm <name>         # remove a stored key (with confirmation)
```

The first generated key automatically becomes the default.

### `groundhog deploy`

Deploy a contract to the active profile's RPC URL. Resolves the WASM
artifact automatically unless `--wasm` is provided, submits a signed
deploy transaction, polls for the receipt, prints the deployed address,
and saves a contract alias into the active profile when running inside a
project.

```bash
groundhog deploy
groundhog deploy --profile testnet
groundhog deploy --json
```

### `groundhog call`

Call a deployed contract method using either a literal address or a
contract alias from the active profile. The CLI resolves entrypoints
from embedded contract specs and local metadata when available, submits
the invocation transaction, and prints returned values plus receipt
status.

```bash
groundhog call my-counter get
groundhog call my-counter increment 5
groundhog call 0xabc123... get
groundhog call my-counter get --json
```

`--dry-run` is not implemented yet.

### `groundhog node`

Start the local development node backed by the same `native_exec`
executor used by SDK tests. Requires installing or building the CLI with
the `dev-node` feature.

```bash
groundhog node
groundhog node --port 9546
groundhog node --accounts 20
```

Human output prints the prefunded accounts and private keys for local
testing. `groundhog node --json` emits the startup payload with
`rpc_url` and `accounts` for scripting.

### `groundhog login`

Configure registry credentials and test executor access.

### `groundhog logout`

Remove stored credentials.

### `groundhog completion <shell>`

Generate shell completions for bash, zsh, fish, elvish, or
powershell.

```bash
groundhog completion zsh > ~/.zfunc/_groundhog
groundhog completion bash > /etc/bash_completion.d/groundhog
```

## Project Configuration

Scaffolded projects include a `groundhog.toml` with profile
support:

```toml
[profile.default]
rpc_url = "http://localhost:9545"
default_key = "dev-0"

[profile.default.contracts]
# counter = "0x..."

[profile.testnet]
rpc_url = "https://testnet.groundhog.network"
```

Select a profile via `--profile`, `GROUNDHOG_PROFILE`, or
the `default` profile is used automatically.

Contract aliases map names to addresses per profile. `deploy` and `call`
resolve aliases from the active profile.

## Global Flags

| Flag | Description |
|------|-------------|
| `-q`, `--quiet` | Suppress status messages |
| `--json` | Emit structured JSON output |

`--profile` is available on commands that interact with project
config (`build`, `test`, `deploy`, `call`, `status`). Keys are
global — not per-profile.

## Templates

| Template | Description |
|----------|-------------|
| `counter` | Counter contract with increment/decrement and tests |
| `stub` | Bare-bones contract with empty init and deploy test |

## AI-Assisted Development

This repo includes a skill file at `docs/cli/SKILL.md` for use with AI coding
assistants. It provides guidance on storage type selection, SDK macros, test
patterns, and concurrency guardrails specific to Groundhog contracts.

To use it, follow your LLM's instructions on where/how to place skills files.
