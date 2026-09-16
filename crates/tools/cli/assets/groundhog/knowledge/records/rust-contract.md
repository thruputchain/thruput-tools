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
RPC tests against an ephemeral local node.

Preserve the scaffold's feature names, RV32 binary wrapper, Cargo overlays, and
self dev-dependency. They are toolchain contracts, not incidental style.

CLI 0.17 uses SDK 0.34.4, protocol-rs 0.36.0, and XDR 0.10.1. Update
these dependencies together when migrating an existing Rust project. SDK
0.34.4 moves to protocol-rs 0.36.0, whose generated x23 clients require
rpc_driver 0.11; its public XDR types are unchanged from 0.34.3, which,
despite its patch number, changed them. Standard interfaces `=0.1.31` use
this family while preserving the native token contract ABI; projects
retaining SDK `=0.34.3` should retain interfaces `=0.1.30`, and SDK `=0.34.2`
interfaces `=0.1.29`.

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
