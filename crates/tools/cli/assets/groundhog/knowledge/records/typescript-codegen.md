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

The build is offline and checks the artifact against no chain. It reports the
code and guest data memory sizes, and warns when the code section is over the
protocol-default 64 KiB. Whether a chain admits the artifact is decided at
deploy: the RPC endpoint rejects a transaction whose artifact exceeds the
chain's `max_rv32_code_bytes` or `max_rv32_memory_bytes` before submitting
it, and `groundhog deploy` reports the violated bound. Guest data memory
defaults to 64 KiB. A contract that needs more sets `initial_memory_bytes`
under `[package.metadata.groundhog]` in its `Cargo.toml` (an integer, or a
string such as `"128K"`); `groundhog build` and `groundhog test` both read
it, size the linker layout from it, and stamp it into the artifact. Do not
raise it for a chain that has not raised `max_rv32_memory_bytes`.
`--stack-size` still adjusts the stack reserve per build.

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

Generated method descriptors sit under their interface's name and major, as in
`ERC20.v0_1.transfer`, beside their `ERC20.v0_1.TransferArgs` and
`ERC20.v0_1.TransferResult` types and the interface's `DeclaredException` and
`Exception` types. They provide typed operations such as:

- `invocation(...)` to encode an application call;
- `call(...)` in the client DX runtime to retain typed result/exception decoders;
- `view(...)` and `viewOutcome(...)` for read-only execution;
- `decodeReturn(...)` and `decodeException(...)` for raw call data;
- `decodeReceipt(...)` and `decodeReceiptOutcome(...)` for a receipt whose root
  method is known.

Generated event descriptors such as `my_token.v1.events.Transfer` provide
`decode(...)`, `filter(...)`, `allFromReceipt(...)`, and
`singleFromReceipt(...)`.

Declared types, enums and events sit under their spec id's path: crate, major,
then module, as in `my_token.v1.errors.TokenError`. The same path names the
type and the value: types live in namespaces, and enum values and event
descriptors in one object per crate, so a binding is erasable TypeScript that
Node's type stripping runs without a build step. A path segment that is a
TypeScript reserved word or `globalThis`, and a crate or interface named
`spec` or `decodeEvent`, take a leading `$`, as in
`my_token.v1.$default.Config`.

Use those generated codecs. Do not create a low-level XDR reader when a binding
already knows the method or event type.

Bindings emit canonical interface/version and ABI/public method names for runtime
diagnostics. They require a client runtime accepting this method metadata. The
client DX runtime implements typed calls, execution and receipt observation;
codegen does not embed wallet or transport behavior. Hashed event topics contain
only the digest, not the original value; generated comments describe their
filtering restriction. Regenerate bindings together with the reviewed client
runtime before adopting the new descriptors.

With the scalar-topic encoding in SDK 0.36.0, `u32`, `u64`, `u128`,
`i32`, `i64`, `i128`, `U256`, `I256` and `bool` topics are directly decoded and
filterable. Integer topics are fixed 32-byte big-endian values with zero/sign
extension, not compact XDR scalar encodings. Strings and byte slices remain
hash-only. Rebuild contracts and regenerate their bindings together when adopting
this encoding; an older artifact's event bytes are not changed by regeneration.
The workspace SDK dependencies and newly scaffolded contracts are pinned to
SDK 0.36.0 so they emit the representation these bindings describe.
