+++
schema_version = 1
id = "groundhog.typescript-codegen"
owner = "groundhog-rpc"
kind = "workflow"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 40
+++
## Build Artifacts and TypeScript Codegen

The Rust contract's `.spec.xdr` is the canonical interface. Build deployable RV32
code and generate bindings together, or generate from an existing spec:

```console
groundhog build --bindings ts
groundhog codegen ts path/to/contract.spec.xdr -o path/to/contract.ts
```

Use installed `--help` for paths/options. Regenerate after API changes; never
maintain a second TypeScript ABI. Plain build warns on codegen-incompatible names;
`--bindings ts` makes them errors. Rename the reported declarations.

Build is offline. It reports code/memory sizes and warns above the default 64 KiB
code limit; deployment enforces max_rv32_code_bytes and max_rv32_memory_bytes.
Guest memory defaults to 64 KiB. Set `initial_memory_bytes` in
`[package.metadata.groundhog]` (integer or `"128K"`) within the chain's limit.
Build/test use it for linker layout and artifact metadata. `--stack-size` sets a
reserve within that memory, not a larger total allowance.

Methods and types use interface/version paths: `ERC20.v0_1.transfer` and
`ERC20.v0_1.TransferArgs`. Descriptors provide:

- `invocation` and `call` to prepare calls;
- `view` and `viewOutcome` for simulation;
- `decodeArgs`, `decodeReturn`, and `decodeException` for known method data;
- `decodeReceipt` and `decodeReceiptOutcome` for a known root method's receipt.

`decodeArgs` consumes the complete input and returns undefined for no arguments.
It verifies byte shape, not contract behavior. This runtime addition needs no new
metadata; the separate spec/namespace migration requires regenerated bindings.

Generated event descriptors such as `my_token.v1.events.Transfer` provide
`decode`, `filter`, `allFromReceipt`, and `singleFromReceipt`. Types, enums, and
events follow the spec id: crate, major, module, declaration. Value objects and
type namespaces share paths, preserving erasable TypeScript and Node type
stripping. Reserved path names, globalThis, and crate/interface names spec or
decodeEvent take a leading `$`. Use generated codecs rather than manual XDR.

Addresses, fixed 32-byte values, `u32`, `u64`, `u128`, `i32`, `i64`, `i128`,
`U256`, `I256`, and `bool` are directly filterable topics. Integers use 32-byte
big-endian values with zero/sign extension, not CXS scalar encoding. Strings
and byte slices expose only a hash, not their preimage.
