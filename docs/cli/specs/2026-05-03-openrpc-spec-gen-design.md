# OpenRPC Spec Generation from jsonrpsee Traits

Generate a machine-readable OpenRPC document from the jsonrpsee
`#[rpc]` trait definitions, following Sui's `sui-open-rpc` pattern.
The checked-in `openrpc.json` artifact is consumed by client-ts CI
for TypeScript type generation/verification.

## Prerequisites

Bump `groundhog-xdr` to `0.11.0` in the workspace `Cargo.toml` and
enable the `schema` feature:

```toml
groundhog-xdr = { version = "=0.11.0", registry = "groundhog", features = ["serde", "xdrs", "schema"] }
```

The `schema` feature adds `#[derive(schemars::JsonSchema)]` to all
XDR types (`BlockHeight`, `SHA256Hash`, `ContractAddress`, `DbHash`,
`Ed25519PublicKey`, etc.).

`groundhog-indexer` already has `schemars` always on, so
`EventQuery`, `EventFilter`, `PaginationCursor`, etc. already derive
`JsonSchema`.

## New Crates

### `crates/open-rpc-macros`

Proc macro crate. Exports a single attribute macro:

```rust
#[open_rpc(namespace = "groundhog", tag = "Core API")]
```

Applied above `#[rpc(client, server, ...)]` on the same trait. The
macro parses the trait, extracts each `#[method(name = "...")]` item,
and generates a companion struct:

```rust
pub struct GroundhogApiOpenRpc;
impl GroundhogApiOpenRpc {
    pub fn module_doc() -> groundhog_open_rpc::Module { ... }
}
```

The generated `module_doc()` body calls
`RpcDocBuilder::method::<ParamType, ReturnType>(name, description)`
for each method. The `ParamType` and `ReturnType` bounds require
`schemars::JsonSchema`, which produces JSON Schema at runtime.

The macro extracts:
- Method name from `#[method(name = "...")]`
- Parameter names and types from the fn signature
- Return type (unwrapping `RpcResult<T>` to `T`)
- Doc comments as method descriptions
- `Option<T>` params marked as not required

### `crates/open-rpc`

Runtime library. Defines:

- **`Module`** — one trait's methods and their schemas.
- **`Project`** — aggregates modules into a complete OpenRPC
  document. Serializes to the OpenRPC 1.2.6 JSON format.
- **`RpcDocBuilder`** — helper that calls
  `schemars::schema_for::<T>()` to produce content descriptors for
  params and return types.

Dependencies: `schemars`, `serde`, `serde_json`.

## Changes to Existing Crates

### `crates/rpc-api`

Add the `#[open_rpc]` attribute to both traits:

```rust
#[open_rpc(namespace = "groundhog", tag = "Core API")]
#[rpc(client, server, namespace = "groundhog")]
pub trait GroundhogApi { ... }

#[open_rpc(namespace = "groundhog_dev", tag = "Dev API")]
#[rpc(client, server, namespace = "groundhog_dev")]
pub trait GroundhogDevApi { ... }
```

Add `groundhog-open-rpc-macros` and `groundhog-open-rpc` as
dependencies.

### `crates/api-types`

Add `#[derive(schemars::JsonSchema)]` to all public structs and
enums:

**Core types:** `TransactionReceipt`, `ExecStatus`, `TxHash`,
`TransactionStatus`, `ChainTip`, `BlockInfo`, `EmittedEvent`,
`IdentifiedEvent`, `EventPage`.

**Dev types:** `DevAccount`, `TransactionDetail`, `StorageDeltaInfo`,
`StorageItemInfo`, `TraceInfo`, `InvocationInfo`, `SyscallInfo`,
`SyscallStatusInfo`, `FrameOutputInfo`.

**Hex wrappers:** `Hex0x<T>`, `HexBytes`, `Topic` — these need
custom `JsonSchema` impls since their serde representation (0x hex
string) doesn't match the inner type's schema. `Hex0x<T>` should
report as `{ "type": "string", "pattern": "^0x[0-9a-f]*$" }`.

Add `schemars` as a dependency.

## Spec Output

A test in `crates/open-rpc` assembles and snapshot-tests the full
document:

```rust
#[test]
fn openrpc_spec_is_up_to_date() {
    let mut spec = Project::new("Groundhog JSON-RPC", env!("CARGO_PKG_VERSION"));
    spec.add_module(GroundhogApiOpenRpc::module_doc());
    spec.add_module(GroundhogDevApiOpenRpc::module_doc());
    let json = serde_json::to_string_pretty(&spec).unwrap();
    insta::assert_snapshot!("openrpc", json);
}
```

The snapshot lives at `crates/open-rpc/snapshots/openrpc.json`. CI
fails if any RPC method signature changes without updating the
snapshot (`cargo insta review` to accept).

## OpenRPC Document Structure

```json
{
  "openrpc": "1.2.6",
  "info": {
    "title": "Groundhog JSON-RPC",
    "version": "0.3.0"
  },
  "methods": [
    {
      "name": "groundhog_sendRawTransaction",
      "summary": "Submit a signed transaction...",
      "tags": [{ "name": "Core API" }],
      "params": [
        {
          "name": "tx_hex",
          "required": true,
          "schema": { "type": "string" }
        }
      ],
      "result": {
        "name": "TxHash",
        "schema": { "$ref": "#/components/schemas/TxHash" }
      }
    }
  ],
  "components": {
    "schemas": {
      "TxHash": { "...schemars output..." },
      "TransactionReceipt": { "..." },
      "ChainTip": { "..." }
    }
  }
}
```

## What This Does NOT Include

- **`rpc.discover` at runtime** — the gateway and local node do not
  serve the spec. The spec is a build artifact only. Runtime serving
  can be added later by wiring the `Project` into the `RpcModule`.
- **TypeScript generation** — the `openrpc.json` is the interface
  contract. TS generation from it is a separate concern in client-ts
  (via `@open-rpc/typings` or a custom script).
- **Examples** — Sui includes hand-written request/response examples
  in their spec. We skip this for now.

## Testing

- Unit tests in `open-rpc` for `Project` serialization and
  `RpcDocBuilder` schema extraction.
- The snapshot test is the integration test — it exercises the full
  pipeline from proc macro through schema generation to JSON output.
- `cargo test --workspace` must pass (the snapshot test enforces spec
  freshness).
