---
name: groundhog-contract
description: >
  Guides Groundhog smart contract development using the current CLI scaffold
  and SDK patterns. Focuses on contract structure, storage type selection,
  test patterns, and concurrency guardrails. Activates when working in a
  project with sdk in Cargo.toml (registry = "groundhog") or
  .cargo/contract.toml present.
---

# Groundhog Contract Development

You are helping a developer write a Groundhog smart contract.
Groundhog contracts are `no_std` Rust libraries compiled to RV32. The SDK provides
storage primitives with specific concurrency semantics. Contract correctness depends heavily on
choosing the right storage type and write pattern.

Use the current scaffold shape as the default source of truth. If older
docs or examples conflict with the local template or current `sdk_rs`
patterns, prefer the scaffold and current SDK examples.

## When This Skill Applies

Activate when the user is working in a directory containing:
- `Cargo.toml` with `sdk` dependency from `registry = "groundhog"`
- `.cargo/config.toml` and `.cargo/contract.toml`

This covers both freshly scaffolded projects and ongoing development.

## Start Here

When generating or editing a contract, default to this workflow:

1. Preserve the scaffold shape unless there is a strong reason to change it.
2. Keep all contract logic in `src/implementation.rs`.
3. Keep `src/lib.rs` minimal: `no_std` gate, mock registration macro,
   module declaration, and public re-exports.
4. Prefer `#[contract_exception]` for contract-facing errors.
5. Prefer `NNInt` or `Map<_, NNInt>` for counters and balances.
6. Write native tests that use `register_mock_contract(&env)`,
   `test_contract.as_hash().clone()`, and `env.transact(...)`.
7. Whenever a method changes, update the tests in the same pass.

## Project Structure

Every scaffolded contract project has this layout:

```text
src/
  lib.rs              # no_std gate, mock registration, re-exports
  implementation.rs   # all contract logic
  main.rs             # rv32 bin wrapper (entrypoints-gated, no_std/no_main)
tests/
  test.rs             # native integration tests
  integration.rs      # RPC tests run by groundhog test --integration
  test_rv32.rs        # default executor-backed RV32 tests
AGENTS.md             # the layout contracts, stated for coding agents
Cargo.toml
build.rs              # activates the RPC starter only for groundhog integration runs
rust-toolchain.toml
.cargo/
  config.toml         # host + rv32 defaults
  contract.toml       # overlay used by groundhog build/test
```

### `src/lib.rs`

Use this shape by default:

```rust
#![cfg_attr(not(feature = "dev"), no_std)]

sdk::declare_mock_contract!();

mod implementation;

pub use implementation::MyContract;
pub use implementation::MyContractClient;
```

Rules:
- Keep the `no_std` gate exactly as shown.
- Always include `sdk::declare_mock_contract!();` so native tests can
  use `register_mock_contract(&env)`.
- Re-export the main contract type.
- Re-export generated client types when tests or callers need them.
- Do not teach or generate `exports::*` or `test_setup` as the default
  pattern. That is stale.

### `src/implementation.rs`

Keep all business logic here:
- contract-facing error types
- contract state storage
- the `#[contract(...)]` impl
- helper functions that support contract methods

### `Cargo.toml`

For a normal scaffolded contract, the baseline is minimal:

```toml
[package]
name = "my-contract"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["rlib"]
doctest = false
test = false

[[bin]]
name = "my_contract"
path = "src/main.rs"
required-features = ["entrypoints"]
test = false
bench = false

[features]
entrypoints = []
dev = ["sdk/dev"]

# Authoritative pin: SCAFFOLD_SDK_VERSION in crates/tools/cli/src/scaffold.rs.
[target.'cfg(target_arch = "riscv32")'.dependencies]
sdk = { version = "0.29.0", registry = "groundhog" }

[target.'cfg(not(target_arch = "riscv32"))'.dependencies]
sdk = { version = "0.29.0", registry = "groundhog", features = ["std"] }

[dev-dependencies]
my-contract = { path = ".", features = ["dev"] }
```

Guidance:
- Start with `sdk` only.
- The feature names `entrypoints` and `dev` are contracts with the
  toolchain, not conventions: the `#[contract]` macro keys guest
  entrypoint codegen on the literal name `entrypoints` (and
  `groundhog build` compiles the rv32 bin with it), while
  `groundhog test` runs host tests with `--features dev`. Do not
  rename either, and do not make `dev` a default feature — a crate
  depending on this one would drag `sdk/dev` (std) into guest builds
  through feature unification.
- The self-referential dev-dependency is deliberate: it enables `dev`
  for `cargo test` builds only, keeping the library no_std for guest
  builds. Do not "simplify" it away.
- `sdk` is declared once per target for the same reason: it requires
  `std` on the host and forbids it on the guest. Declaring the host
  dependency with `features = ["std"]` is what lets plain `cargo build`
  / `cargo check` (and so rust-analyzer) succeed without `dev`. Do not
  collapse the two entries into one `[dependencies]` line.
- If another contract crate depends on this one, forward `dev` through
  the dependent's own feature (`dev = ["sdk/dev", "my-contract/dev"]`),
  the way the SDK's erc20 example forwards its interface crates'
  `dev` features.
- Add `thiserror` only if the contract actually needs it.
- Add `xdrs` only if the contract needs manual XDR derives or other
  direct `xdrs` usage.
- Do not cargo-cult extra dependencies into simple contracts.

## Error Types

Prefer `#[contract_exception]` for contract-facing errors:

```rust
use sdk::*;

#[contract_exception]
pub enum Error {
    NotAuthorized = 1,
    InsufficientBalance = 2,
}
```

Rules:
- Prefer `#[contract_exception]` over manually deriving `ReadXDR`,
  `WriteXDR`, and `Exn` for the common case.
- Use explicit discriminants starting at `1`.
- Keep contract-facing error enums small and stable.
- Use `throw(Error::Variant)` for contract-level aborts.

If the contract needs richer custom serialization behavior, follow a
current `sdk_rs` example rather than inventing a new pattern.

## Storage Struct

```rust
#[derive(Debug)]
#[sdk::storage]
pub struct MyContract {
    balance: NNInt,
    owner: Bytestring,
    accounts: Map<ContractAddress, NNInt>,
}
```

Rules:
- Mark contract state with `#[sdk::storage]`.
- Choose storage types based on concurrency behavior, not just shape.
- Prefer a single top-level state struct even for simple contracts.

## Contract Macro

Use this form by default:

```rust
#[contract(throws = Error)]
impl MyContract {
    pub fn initialize(&mut self, owner: ContractAddress) {
        self.owner.set(owner.as_bytes());
    }

    pub fn deposit(&mut self, amount: u64) {
        self.balance.strict_inc(amount as i128);
    }

    pub fn get_balance(&self) -> u64 {
        self.balance.try_get().unwrap_or(0) as u64
    }
}
```

### Macro arguments

- `throws = ErrorType` marks the contract-facing error type.
- `client = ClientName` is optional. The default generated client name is
  `<ContractType>Client`, so only specify `client = ...` when you need a
  non-default name.

### Method rules

- `&mut self` loads state, applies writes, and stores updated state.
- `&self` loads state read-only.
- No `self` means no contract state is loaded.
- Arguments and return values must be XDR-serializable.
- Do not use `#[init]` as the default pattern. Current scaffolded
  contracts simply expose an `initialize` method when needed.

### Generated output

The macro generates:
- a typed client, usually `<ContractType>Client`
- entrypoints used by the deployed contract

A caught exception is not decoded into a generated enum: `try_*` client
methods return `CallError`, matched by selector with `match_exn!`.

For native tests, the important helper comes from
`sdk::declare_mock_contract!()`: `register_mock_contract(&env)`.

## Test Patterns

When adding or changing contract methods, update the tests in the same
pass.

### Native test structure

Use this pattern by default:

```rust
use my_contract::{register_mock_contract, MyContractClient};
use sdk::*;

static NONCE: SHA256Hash =
    sdk::sha256_static!("MY_CONTRACT_NONCE");

fn setup() -> (TestEnvironmentHandle, SHA256Hash) {
    let env = TestEnvironment::no_cost();
    let test_contract = register_mock_contract(&env);
    let contract_hash = test_contract.as_hash().clone();
    (env, contract_hash)
}

#[test]
fn test_initialize_and_deposit() {
    let (env, contract_hash) = setup();

    env.transact(|tx| {
        let caller = <ContractAddress as Named>::named("test_initialize_and_deposit");
        tx.call(&caller).run(move || {
            let addr = deploy_contract(&NONCE, &contract_hash);
            let client = MyContractClient::new(&addr);

            client.initialize(/* args */);
            client.deposit(100);
            assert_eq!(client.get_balance(), 100);
        })
    })
    .assert_success();
}
```

### Test infrastructure

- `TestEnvironment::no_cost()` creates a test environment with no
  storage-cost pressure.
- `register_mock_contract(&env)` registers the package for native tests.
- `test_contract.as_hash().clone()` gives the package hash used by
  `deploy_contract(...)`.
- `env.transact(|tx| tx.call(&caller).run(move || { ... })).assert_success()`
  executes the test body with contract semantics and commits on success.
- `MyContractClient::new(&addr)` constructs the generated typed client.

### Test guidance

- Write one test per behavior or edge case.
- Test normal mutation flows and error cases.
- For `NNInt`, explicitly test underflow behavior.
- Use distinct nonces when deploying multiple contract instances.
- Keep native tests simple and explicit; they are the fastest feedback loop.

### Executor-backed tests

- Keep `tests/test_rv32.rs` as the default executor-backed path.
- Use the scaffolded pattern based on `load_rv32_contract(...)`.
- Native tests and executor-backed tests serve different purposes; do not
  replace one with the other.

## Storage Types and Concurrency

### The execution model

All transactions in a block read from the same frozen snapshot of the
previous block state. They do not see one another's writes. Each
transaction produces deltas, and the runtime merges compatible deltas.
Conflicting transactions are excluded and returned to the mempool.

This means your storage type and write method directly determine whether
concurrent transactions can coexist.

### `NNInt`

Non-negative integer. This is the default choice for counters, balances,
and amounts.

```rust
// Reading
self.value.try_get() -> Option<i128>
self.value.get() -> i128

// Commuting writes
self.value.strict_inc(delta)
self.value.inc(delta)

// Absolute write
self.value.set(value)
```

Rules:
- Prefer `strict_inc()` for balances and counters.
- `strict_inc()` and `inc()` commute with one another.
- `set()` conflicts with other writes unless all callers set the same value.
- For decrements, use `self.value.strict_inc(-(amount as i128))`.
- For safe reads, use `self.value.try_get().unwrap_or(0)`.

### `Map<K, V>`

Key-value store. Different keys are independent conflict domains.

```rust
self.accounts.get(&key) -> V
self.accounts.try_get(&key) -> Option<V>
self.accounts.set(&key, &value)
self.accounts.delete(&key)
```

Rules:
- Use `Map<K, NNInt>` for per-user balances or counters.
- Use `Map<K, Bytestring>` for per-key metadata written infrequently.
- Same-key concurrency follows the rules of the value type.
- Nested maps are valid when you need compound-key independence.

### `Bytestring`

Raw bytes with last-write-wins behavior.

```rust
self.owner.set(value.as_bytes())
self.owner.get() -> Box<[u8]>
self.owner.get_sized::<N>() -> [u8; N]
self.owner.is_empty() -> bool
```

Rules:
- Use `Bytestring` for ownership, config, and metadata.
- Do not use it for hot mutable state like balances or counters.
- Concurrent writes to the same location conflict.

### `Handle<T>`

Typed reference to a serializable value.

```rust
let val = self.data.load();
let mut val = self.data.load_mut();
```

Rules:
- Use `load()` and `load_mut()`.
- Do not instruct users to call `.get()` or `.set()` directly.

## Storage Type Selection Guide

| Access Pattern | Use | Why |
|---|---|---|
| Balance / counter | `NNInt` | `strict_inc` commutes |
| Per-user balances | `Map<K, NNInt>` | independent keys + commuting values |
| Per-user metadata | `Map<K, Bytestring>` | independent keys, rare writes |
| Owner / admin address | `Bytestring` | written rarely, read often |
| Config / settings | `Bytestring` | infrequent updates |
| Approval mappings | `Map<K1, Map<K2, NNInt>>` | independent `(K1, K2)` pairs |
| Append-style event data | `RetrocausalQueue` | inserts commute |

## SDK Utilities

```rust
use sdk::*;

get_caller() -> ContractAddress
get_current_address() -> ContractAddress

ensure(predicate, error)
throw(error)

deploy_contract(&nonce, &hash) -> ContractAddress

static NONCE: SHA256Hash = sha256_static!("MY_NONCE");
```

Use these helpers directly rather than reimplementing authorization,
deployment, or hash setup logic.

## Anti-Patterns to Flag

Always flag these when you see them:

1. `NNInt::set()` on balances or counters. Use `strict_inc()` or `inc()`.
2. `Bytestring` for frequently updated state. Use `NNInt` or `Map<_, NNInt>`.
3. Retry logic in contract code. The runtime and mempool handle exclusion.
4. `Handle<T>.get()` or `.set()` directly. Use `load()` or `load_mut()`.
5. `NNInt::get()` without handling unset state when zero/default is expected.
6. Teaching stale native test patterns like `test_setup::register(...)` or
   `env.setup_create_contract(&contract)` as the default path.
7. Teaching `#[init]` as a required or default SDK attribute.

## Build and Test Commands

Use these commands by default:

```bash
cargo test
groundhog build
groundhog test
groundhog test --integration
```

Meaning:
- `cargo test` runs native tests using the `dev` feature.
- `groundhog build` performs the default RV32 build with the contract-specific
  cargo overlay.
- `groundhog test` runs executor-backed testing for the RV32 path.
- `groundhog test --integration` starts an ephemeral hog-devnode local node,
  deploys the built contract
  with the runtime manifest's lowest-index `local_user`, and runs `tests/integration.rs` with
  `GROUNDHOG_RPC_URL`, `GROUNDHOG_CONTRACT_ADDRESS`,
  `GROUNDHOG_PROFILE=integration`, `GROUNDHOG_DEFAULT_KEY=dev-1` by default, and an
  optional `GROUNDHOG_RUNTIME_MANIFEST`. The profile and key values describe
  the deployment context; integration tests connect directly to the RPC URL.

## Guidance for LLMs

When generating contract code:

- Prefer the smallest contract that demonstrates the required behavior.
- Choose storage types for concurrency semantics first, ergonomics second.
- Keep public methods narrow and typed.
- Write the contract and tests together.
- If the user asks for a token, counter, allowance map, or balance system,
  default to `NNInt` or `Map<_, NNInt>`.
- If the user asks for ownership or configuration, default to `Bytestring`
  or another rarely-written storage value.
- If the user needs custom serialization or more advanced interfaces,
  follow a current `sdk_rs/example-contracts` pattern instead of inventing
  a new one.
