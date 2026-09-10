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

`groundhog build` compiles and lowers the deployable RV32 artifact. A contract
spec produced by the Rust contract is the canonical interface for generated
bindings.

The guest layout defaults to the SDK linker script's stack reserve and a
64 KiB data address space. `--stack-size` overrides the reserve, and
`--memory-size` stamps a larger address space into the artifact (and offers
it to the linker script as `__memory_size`); `--max-code-bytes` raises the
code limit the lowering enforces. `groundhog test` takes the same flags, so
the artifact under test is the one that would deploy. A chain admits an
artifact only up to its configured `max_rv32_memory_bytes` and
`max_rv32_code_bytes`, 64 KiB each unless the chain raised them, so keep the
defaults unless the target chain says otherwise.

Generate a TypeScript binding as part of the build:

```console
groundhog build --bindings ts
```

Or generate it from an existing spec:

```console
groundhog codegen ts path/to/contract.spec.xdr -o path/to/contract.ts
```

Run the installed commands with `--help` for current paths and options. Re-run
codegen after changing exported methods, events, exceptions, or contract types.
Do not hand-maintain a TypeScript ABI beside the Rust contract spec.

Generated method descriptors provide typed operations such as:

- `invocation(...)` to encode an application call;
- `view(...)` and `viewOutcome(...)` for read-only execution;
- `decodeReturn(...)` and `decodeException(...)` for raw call data;
- `decodeReceipt(...)` and `decodeReceiptOutcome(...)` for a receipt whose root
  method is known.

Generated event descriptors such as `PingEvent` provide `decode(...)`,
`filter(...)`, `allFromReceipt(...)`, and `singleFromReceipt(...)`.

Use those generated codecs. Do not create a low-level XDR reader when a binding
already knows the method or event type.
