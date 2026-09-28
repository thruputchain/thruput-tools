+++
schema_version = 1
id = "groundhog.rust-contract"
owner = "groundhog-rpc"
kind = "workflow"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 30
+++
## Rust Contract Path

Start from the installed scaffold instead of recreating its Cargo and RV32
configuration:

```console
groundhog new my-contract
cd my-contract
cargo test
groundhog build
groundhog test
```

Use `cargo test` for fast native tests. Use `groundhog test` for the generated
RV32 artifact. They exercise different paths and neither substitutes for the
other. The scaffolded SDK downloads its test executor from public storage; no
FFI download credentials are required. Cargo registry authentication follows
the early-access setup instructions. Use `groundhog test --integration` for
RPC tests against an ephemeral local node. In a multi-contract Cargo workspace,
`groundhog test --workspace` runs the RV32 tests of every member that has a
`tests/test_rv32.rs` target and reports them together.

Preserve the scaffold's feature names, RV32 binary wrapper, Cargo overlays, and
self dev-dependency. They are toolchain contracts, not incidental style.

That self dev-dependency puts the project name in the extern prelude of every
test target, so `groundhog new` refuses names that would shadow something the
generated project needs: Rust's built-in crates (`test`, `core`, `std`,
`alloc`, `proc-macro`), Rust keywords, the scaffold's own dependencies (`sdk`,
`tokio`, `groundhog-json-rpc-client`), Cargo's build directory names (`build`,
`deps`, `examples`, `incremental`), and reserved Windows device names. The
error names the collision; pick another name rather than patching the
generated manifest.

This CLI uses SDK 0.40.0, standard interfaces 0.1.40, protocol-rs 0.42.0,
and XDR 0.10.1. Update the SDK and interface dependencies together when
migrating an existing Rust project.

The normal starter contract is a counter, one of the narrower cases where
`NNInt` is appropriate. Do not copy this storage choice for token balances or
other ordinary nonnegative amounts; use `Balance` for those.

The contract shape is:

```rust
use sdk::storage::NNInt;
use sdk::*;

#[derive(Debug)]
#[sdk::storage]
pub struct Counter {
    value: NNInt,
}

#[contract]
impl Counter {
    pub fn increment(&mut self, amount: u64) {
        self.value.strict_inc(amount as i128);
    }

    pub fn get(&self) -> u64 {
        self.value.try_get().none_if_missing().unwrap_or(0) as u64
    }
}
```

Keep contract logic in `src/implementation.rs`; keep `src/lib.rs` focused on
the `no_std` gate, mock registration, module declaration, and public
re-exports. Prefer `#[contract_exception]` for contract-facing exceptions.
Update native and RV32 tests with behaviour changes.

Before inventing a macro attribute or test helper, inspect the scaffold and the
public Rustdoc for the SDK version the project actually uses.
