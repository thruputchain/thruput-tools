# TypeScript Binding Codegen

**Status:** Shipped. The current implementation lives in
`crates/tools/cli/src/codegen.rs` and is exposed through `groundhog codegen ts` plus
`groundhog build --bindings ts`.

Generate typed TypeScript bindings from Groundhog contract specs.
Like wagmi codegen for Ethereum ABIs, but for Groundhog contracts.

## Entry Points

### `groundhog codegen ts <source>`

Standalone command. `<source>` is a `.wasm` file (spec extracted from
the embedded `groundhogspecv0` custom section) or a `.spec.xdr` file
(raw XDR-encoded `ContractSpec`). Writes generated TS to stdout by
default; `--output <path>` writes to a file. Supports `--json` like
all CLI commands.

### `groundhog build --bindings ts`

After a successful build, generates `<contract>.ts` next to the `.wasm`
and `.spec.json` in the target directory. If the contract has no
embedded spec, warns and skips (same as the existing `.spec.json`
behavior). Only `ts` is supported for now; additional languages can be
added later.

### `.spec.xdr` sidecar

`groundhog build` also writes a `.spec.xdr` file alongside the
`.spec.json`. This is the raw XDR-encoded `ContractSpec` bytes — the
same data that lives in the `groundhogspecv0` WASM custom section,
but as a standalone file. This gives external tools (including
`groundhog codegen ts`) a portable, binary-stable input that doesn't
require a full WASM binary.

## Generated Output

One `.ts` file per contract spec. Multi-interface specs produce one file
with all interfaces' methods.

### File Structure

In order:

1. **Header comment** — source file, interface name(s)/version(s),
   `DO NOT EDIT. Re-run codegen to update.`
2. **Imports** — from `@groundhogchain/protocol`,
   `@groundhogchain/contract-spec`, `@groundhogchain/client`
3. **Entrypoint ID constants** — `const entrypoints = { method_name:
   contractEntrypointID(...) } as const`
4. **Named TS types** — exported interfaces, enums, and discriminated
   union types for all `type_defs` in the spec
5. **Inline spec literal** — `export const spec: ContractSpec = { ... }`
   — the full spec object so the file is self-contained for
   encode/decode at runtime
6. **One function per method** — typed args, returns `Invocation`

### Type Mapping — Scalars

| Contract type | TS type        |
|---------------|----------------|
| U8            | `bigint`       |
| U32           | `bigint`       |
| I32           | `bigint`       |
| U64           | `bigint`       |
| I64           | `bigint`       |
| U128          | `bigint`       |
| I128          | `bigint`       |
| Bool          | `boolean`      |
| String        | `string`       |
| Bytes         | `Uint8Array`   |
| Address       | `ContractAddress` |
| Void          | omitted / `void` |

### Type Mapping — Composites

| Contract type   | TS type           |
|-----------------|-------------------|
| Option\<T\>     | `T \| null`       |
| Vector\<T\>     | `T[]`             |
| Array\<T, N\>   | `T[]`             |
| Tuple\<A, B\>   | `[A, B]`          |
| Named           | reference to generated type |

Size bounds on vectors and arrays are not encoded in the TS type.
They are enforced at runtime by the XDR codec layer
(`VarArrayCodec`/`FixedArrayCodec` from `@groundhogchain/xdr`),
consistent with the existing protocol-ts convention.

### Type Mapping — Named Types

| Contract type def | Generated TS |
|-------------------|--------------|
| Struct | `export interface Foo { field1: Type1; field2: Type2; }` |
| Enum   | `export enum Foo { Variant1 = 1, Variant2 = 2 }` (real TS enum with numeric discriminants, matches protocol-ts convention) |
| Union  | discriminated union on `case` field (see below) |

Union cases map to TS per their payload shape:

```ts
// Void case — no value field
export type Result =
  | { case: "Err" }
  // Tuple case — value is a positional tuple
  | { case: "Ok"; value: [bigint] }
  // Struct case — value is a named-field object
  | { case: "Detail"; value: { code: bigint; msg: string } };
```

Recursive types are supported — struct fields and union cases can
reference their own type via Named references.

### Codec pre-computation

After the spec literal, the generated file builds the named codecs
once and reuses them in every function:

```ts
const namedCodecs = buildNamedCodecs(spec.type_defs);
```

This avoids rebuilding the codec map on every `encodeCalldata` call.

### Generated Function Shape

```ts
export function transfer(
  contract: ContractAddress,
  gasLimit: GasAmount,
  args: { to: ContractAddress; amount: bigint },
): Invocation {
  return createInvocation({
    invoked_address: contract,
    entrypoint: entrypoints.transfer,
    gas_limit: gasLimit,
    calldata: encodeCalldata(spec, "ERC20", "transfer", [args.to, args.amount], namedCodecs),
  });
}
```

- Method names are converted from snake_case to camelCase.
- Zero-arg methods omit the `args` parameter.
- Return type is `Invocation`. Decoding the contract return value is
  a separate concern handled by the caller via `decodeCalldata`.
- Each function references the inline `spec`, `entrypoints`, and
  pre-built `namedCodecs` constants, so the file is self-contained.

## Implementation

Pure Rust codegen in the CLI. The codegen engine walks the
`ContractSpec` (from `groundhog_contract_spec`) and emits TS text
via string building. No Node/prettier dependency.

Whether the codegen logic lives in the CLI crate, the existing
spec-tool crate, or a new crate is an implementation detail to be
decided during development.

### Spec Loading

The codegen reuses the existing spec extraction in
`crates/tools/cli/src/contract_spec.rs`:

- `.wasm` input: `load_contract_spec` extracts from the embedded
  `groundhogspecv0` custom section
- `.spec.xdr` input: read the file bytes and XDR-decode directly
  into a `ContractSpec`

The input format is determined by file extension.

### Error Cases

- No spec found in `.wasm` → error with hint to run `groundhog build`
- `.spec.xdr` decode failure → error with path and decode details
- Unknown/unsupported type in spec → error naming the type
  (fail rather than silently omit)

## Imports in Generated Code

```ts
import type {
  ContractAddress,
  GasAmount,
  Invocation,
  Calldata,
  ContractEntrypointID,
} from "@groundhogchain/protocol";
import {
  contractEntrypointID,
  calldata,
  gasAmount,
} from "@groundhogchain/protocol";
import { fromHex } from "@groundhogchain/protocol/utils";
import {
  ContractTypeKind,
  ContractTypeDefKind,
  contractSpecString,
} from "@groundhogchain/contract-spec";
import type { ContractSpec } from "@groundhogchain/client";
import { encodeCalldata } from "@groundhogchain/client";
import { buildNamedCodecs } from "@groundhogchain/client";
import { createInvocation } from "@groundhogchain/client";
```

Exact import sets are trimmed to what the specific contract needs
(e.g., `ContractUnionCaseKind` only when the spec has unions).

## Examples

See the hand-written reference implementations in `../client-ts`:

- `src/contracts/erc20.ts` — ERC20 with enum error type, multiple
  methods with Address/U128 args
- `src/contracts/recursive_type.ts` — struct types with recursive
  Option\<Named\> and Vector\<Named\> fields
