# TypeScript Binding Codegen Upgrade

**Date:** 2026-08-14 **Status:** Codegen implemented; coordinated rollout pending

## Goal

Generate TypeScript bindings that preserve method arguments, return values, declared aborts, and identity through invocation and typed views, while strictly decoding contract events.

The first release targets integration code demonstrated by the swap app: anonymous method types, runtime method-name lookup, handwritten return and abort decoding, fixed-length repair, positional direct-topic filters, and receipt event scanning.

## Scope

Update `crates/tools/cli/src/codegen.rs` and its tests. Both existing entrypoints continue to use the same generator:

```text
groundhog codegen ts <source>
groundhog build --bindings ts
```

Generated files add:

- corrected TypeScript projections for supported ABI types;
- exported method argument and result types;
- typed method descriptors for invocation, return, abort, and view workflows;
- strict event decoders with semantic direct-topic types;
- named event filters for directly represented topics;
- receipt event extraction helpers;
- safe source escaping and collision checks for declarations emitted in this release.

## Deferred Work

This release does not generate:

- bound contract clients;
- typed root-receipt method decoders;
- event encoders;
- filters that encode hashed topic preimages;
- camelCase projections for struct, union struct, or event fields;
- exhaustive ABI graph and semantic validation;
- a public codec composition DSL;
- decoder depth and materialization budgets.

These features can build on descriptors and corrected codecs without changing the first-release contract.

## Non-goals

This work does not:

- change the contract spec, CXS encoding, entrypoint derivation, CLI command surface, or contract artifact formats;
- add RPC methods, pinned-height views, transaction polling, pagination, or history policy;
- infer method mutability, gas policy, or event-to-method relationships;
- recover hashed event topic preimages;
- preserve incorrect enum, union, byte-container, or open fixed-array representations through compatibility aliases.

## Correctness Requirements

The release fixes these existing mismatches:

1. Generated unions use `{ case }`, while runtime named codecs use `{ discriminant }`.
2. Generated enums contain numbers, while runtime codecs use bigint and unsigned discriminants.
3. ABI enum and union discriminants are signed `i32` values.
4. U8 vectors and arrays are typed as `Uint8Array` but decode as `bigint[]`.
5. Bare `U8` is valid in the ABI but rejected by codegen.
6. Fixed arrays lose compile-time length.
7. Address and fixed-32 event topics lose semantic types.
8. Source strings can produce invalid TypeScript because they are not escaped for their output context.

The matching client runtime must land with this generator. Generated types must describe the values its codecs accept and return.

## Decisions

| #   | Decision                                                                                      | Rationale                                                                                |
| --- | --------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| D1  | Generate each method as one top-level `<Interface>.<method>` descriptor.                      | One canonical API covers invocation, decoding, and views without compatibility aliases.  |
| D2  | Generate named args and result types for every method.                                        | Applications can reuse method shapes without duplication.                                |
| D3  | Generate bigint const objects plus value-union types for enums.                               | TypeScript enums cannot contain bigint values.                                           |
| D4  | Use case-name unions as the only generated and runtime representation.                        | The generated type and codec remain symmetric.                                           |
| D5  | Preserve fixed-array lengths in public types and runtime codecs.                              | Compile-time and runtime checks cover typed and decoded values.                          |
| D6  | Project U8 vectors as `Uint8Array` and fixed U8 arrays as `FixedBytes<N>`.                    | Byte containers use one representation.                                                  |
| D7  | Preserve ABI names for struct, union struct, event topic, and event payload fields.           | A full naming migration adds projection codecs without improving typed view correctness. |
| D8  | Keep existing camelCase method names and method argument objects.                             | This is already the public generated convention.                                         |
| D9  | Generate named filters only for direct `Address` and `Array<U8, 32>` topics.                  | These types cover the swap filters without generic hash encoding.                        |
| D10 | Strict decoders reject malformed matching data and trailing bytes.                            | Known ABI data must decode completely.                                                   |
| D11 | Treat enum, union, byte, fixed-array, and descriptor changes as coordinated breaking changes. | Compatibility shims would preserve incorrect representations.                            |

## Canonical Type Projection

| Contract type                                    | Generated TypeScript        |
| ------------------------------------------------ | --------------------------- |
| `Void`                                           | `void`                      |
| `Bool`                                           | `boolean`                   |
| `U8`, `U32`, `I32`, `U64`, `I64`, `U128`, `I128` | `bigint`                    |
| `String`                                         | `string`                    |
| `Bytes`                                          | `Uint8Array`                |
| `Address`                                        | `ContractAddress`           |
| `Option<T>`                                      | `T \| null`                 |
| `Vector<U8>`                                     | `Uint8Array`                |
| `Vector<T>`                                      | `T[]`                       |
| `Array<U8, N>`                                   | `FixedBytes<N>`             |
| `Array<T, N>`                                    | exact-length readonly array |
| `Tuple<A, B>`                                    | `readonly [A, B]`           |
| `Named`                                          | generated named type        |

Use `FixedArray<T, N>` when expanding a concrete tuple would produce disproportionate source. Runtime codecs reject incorrect lengths. U8 container decoders return `Uint8Array`.

`Void` is valid only as a method return. A void union arm uses `UnionCaseVoid`. `SignedToken` remains unsupported until the client runtime provides its codec; fail contextually rather than widening to `unknown`.

### Enums

Generate:

```ts
export const ERC20Error = {
  AlreadyInitialized: 1n,
  TooLarge: 3n,
} as const;

export type ERC20Error = (typeof ERC20Error)[keyof typeof ERC20Error];
```

Codecs use signed `i32` and reject undeclared values on encode and decode. Negative and zero values are valid when declared.

### Unions

Generate:

```ts
export type SwapOp =
  | { case: "AddLiquidity"; value: readonly [AddLiquidityRequest] }
  | { case: "SwapExactIn"; value: readonly [SwapExactInRequest] }
  | { case: "Idle" };
```

Codecs map case names to signed discriminants. Reject unknown cases, undeclared discriminants, missing payloads, payloads on void cases, and invalid tuple or struct payloads. Preserve recursive named types and ABI field names.

## Generated Method API

### Named types and aborts

For every method, export deterministic interface-qualified types:

```ts
export interface ERC20GetBalanceOfArgs {
  addr: ContractAddress;
}

export type ERC20GetBalanceOfResult = bigint;
```

Zero-argument methods export `...Args = void`; void methods export `...Result = void`.

Generate one declared-abort union per interface because the ABI declares `throws` at interface scope:

```ts
export type ERC20DeclaredAbort = {
  kind: "declared";
  exception: "ERC20Error";
  selector: 0xb19947acb2fa14ffn;
  value: ERC20Error;
};

export type ERC20Abort = ContractAbort<ERC20DeclaredAbort>;
```

Unknown selectors preserve the selector and complete raw abort bytes. Declared abort decoding consumes the selector and value completely. Faults remain separate from aborts.

### Descriptors

Generate one descriptor per method:

```ts
const codecs = createContractCodecContext(spec.type_defs);

export const ERC20 = {
  getBalanceOf: defineContractMethod<
    ERC20GetBalanceOfArgs,
    ERC20GetBalanceOfResult,
    ERC20DeclaredAbort
  >({
    entrypoint: entrypoints.ERC20.getBalanceOf,
    args: [
      {
        abiName: "addr",
        publicName: "addr",
        type: spec.interfaces[0].methods[0].args[0].ty,
      },
    ],
    returnType: spec.interfaces[0].methods[0].return_type,
    throws: generatedExceptionMetadata,
    codecs,
  }),
} as const;
```

The metadata contract is:

- each argument has ABI name, public camelCase name, and direct `ContractType`;
- the result has a direct `ContractType`;
- each exception has selector, `ContractType`, and a label equal to its named type or `null` for supported non-named types;
- every descriptor receives the module's shared `ContractCodecContext`.

It must not require a public reader-aware codec DSL. Reject duplicate exception selectors.

Each descriptor exposes:

- `invocation(contract, gasLimit, args?)`;
- strict `decodeReturn(data)`;
- `decodeAbort(data)`;
- typed non-throwing `viewOutcome({ client, contract, caller?, args? })`;
- ergonomic throwing `view({ client, contract, caller?, args? })`.

Descriptor construction may resolve internal codecs once. Descriptor operations must not search a `ContractSpec` by interface or method name. Do not generate invocation-builder aliases or a second encoding path.

Views preserve response height and raw emitted events. Return and abort decoders reject trailing bytes. Transport and RPC errors propagate unchanged.

## Generated Event API

Keep the same-named event type/value pattern and combined `decodeEvent` dispatcher.

Each event wrapper calls `defineContractEvent` with its event name, signature, ordered topic metadata, ordered payload metadata, and the module's shared `ContractCodecContext`. Topic metadata identifies direct address, direct fixed-32, or raw hash representation. Property names remain ABI names.

Generate an `<EventName>Filter` type containing only directly represented named topics. Each property accepts its value or `{ oneOf: readonly [Value, ...Value[]] }`. The generated filter input places that type under `topics`, separate from `contractAddresses`.

Each generated event interface extends `DecodedContractEvent<"EventName", Topics, Fields>`. Its same-named value calls `defineContractEvent<"EventName", EventName["topics"], EventName["fields"], EventNameFilter>` and therefore exposes typed decode, filter, `allFromReceipt`, and `singleFromReceipt` methods.

### Decoding

For each event:

1. compare topic 0 before structural validation;
2. return `null` for another signature;
3. require the exact declared topic count for a matching signature;
4. project `Address` topics as `ContractAddress`;
5. project `Array<U8, 32>` topics as `FixedBytes<32>`;
6. leave non-recoverable topics as raw `Uint8Array`;
7. decode payload fields in declaration order and reject trailing bytes.

Event topic and payload property names remain exact ABI names.

### Direct-topic filters

Generate `.filter(...)` for every event and return the client's existing `EventFilter`. Every filter accepts contract addresses and inserts the signature. Events with direct `Address` or `Array<U8, 32>` topics additionally expose those fields under a named `topics` object. The helper:

- requires non-empty `contractAddresses`;
- inserts the signature at topic 0;
- maps named direct topic fields to their declared positions;
- accepts one value or `{ oneOf: readonly [T, ...T[]] }` per field;
- leaves omitted fields unconstrained;
- rejects empty `oneOf` and non-32-byte values.

Do not expose typed preimage fields for hashed topic types. Consumers can use raw event queries until generic topic hashing is implemented.

### Receipt extraction

Generate:

```ts
RequestAccepted.allFromReceipt(receipt, {
  contractAddress: driverAddress,
});

RequestAccepted.singleFromReceipt(receipt, {
  contractAddress: driverAddress,
});
```

Emitter scoping is optional. Receipt helper results retain the emitting `contractAddress`. `allFromReceipt` preserves receipt order, ignores other signatures, and throws for malformed matching events. `singleFromReceipt` requires exactly one result and reports the actual count.

## Safe Source Emission

Before emission, validate and escape every source fragment emitted by this release.

Required checks:

- context-safe escaping for ABI string literals, union case literals, comments, and `source_name`;
- valid non-empty TypeScript identifiers for interfaces, named types, events, generated method types, public methods, and public method arguments;
- safely quoted and escaped property keys for ABI-named struct, union struct, event topic, and event payload fields;
- duplicate field or member names within each emitted struct, union arm, enum, and event object;
- duplicate event signatures;
- rejection of unsafe emitted object keys such as `__proto__`;
- collisions among imports, interface objects, descriptors, named method types, event values, and event unions;
- method and argument collisions after camelCase conversion;
- unresolved named types used by emitted declarations;
- supported reachable types;
- the protocol event topic-count limit;
- deterministic output for identical input.

Preserve ABI declaration order. Errors identify the relevant ABI location and conflicting names.

Exhaustive duplicate semantic validation, guarded-recursion classification, and forecasting names for deferred APIs are outside this release.

## Compatibility and Release

Do not emit legacy invocation builders, enum aliases, discriminant-union overloads, widened fixed arrays, or a legacy mode.

This release changes:

- enums to bigint const/type pairs;
- unions to case-name values;
- U8 containers to `Uint8Array`;
- fixed arrays and bytes to length-aware types;
- methods to add named types and descriptors;
- direct event topics to semantic types.

Struct, union struct, event topic, and event payload property names remain ABI-named.

External bindings import published package entrypoints. The client repository owns a required regeneration script that invokes the CLI generator for curated bindings, rewrites only known client self-import module specifiers to repository-relative imports, formats the result, and fails on unexpected generated changes. The generator does not add a second import mode.

Release the generator and client runtime together. Regenerate curated ERC20 and recursive bindings plus application-owned swap bindings with the coordinated versions.

## Implementation Boundaries

`codegen.rs` owns:

- safe, deterministic source emission;
- generated static types and direct metadata;
- generated descriptor and event declarations;
- entrypoint IDs and embedded ABI metadata.

The client runtime owns:

- corrected dynamic codecs and fixed-value validation;
- complete-consumption helpers;
- method descriptor, view, and abort behavior;
- strict event decoding, direct-topic filter, receipt extraction, and byte-equality primitives;
- shared public outcome and error types.

Generated files call shared runtime primitives. They do not duplicate view execution, strict decoding, receipt scanning, or byte comparison.

## Testing

### Generator tests

Cover:

- supported scalar and composite projections, including bare U8;
- fixed bytes and fixed arrays;
- bigint enums with signed values;
- case-name unions with void, tuple, and struct arms;
- recursive named types;
- method args/results, zero args, void results, and multiple interfaces;
- declared abort metadata and duplicate selectors;
- strict event decoder generation;
- duplicate event signature rejection;
- direct-topic filters and receipt helpers;
- source escaping, emitted identifier validation, relevant collisions, and deterministic output.

Use structural or snapshot assertions for complete generated sections. Error tests assert contextual diagnostics rather than only `is_err()`.

### Compatibility fixtures

The codegen repository owns source specs and golden output for focused ERC20, recursive, and typed-contract fixtures. The client repository owns runtime and type fixtures generated from those shapes. The `groundhog-examples` repository owns real swap bindings and migration tests.

The coordinated client tests prove:

- argument and return round trips;
- signed enum and union values;
- U8 containers and fixed lengths;
- strict return, abort, and event decoding;
- view return, abort, and fault outcomes with height and events;
- direct-topic filters;
- emitter-scoped receipt extraction and cardinality.

Run:

```bash
cargo fmt --check
cargo test -p groundhog-cli
cargo clippy -p groundhog-cli --all-targets -- -D warnings
```

The coordinated client must pass format, typecheck, test, and build. The swap application must regenerate its bindings and pass relevant integration tests.

Release order is client runtime implementation, codegen implementation, curated client regeneration, swap regeneration and migration, client publication, then CLI generator release.

## Acceptance Criteria

The release is ready when:

1. generated public types match runtime codec values for every supported ABI type;
2. applications can invoke, decode, and execute typed views without spec traversal or method-name strings;
3. fixed arrays, fixed bytes, and direct address topics require no handwritten length repair;
4. generated event decoders are strict and expose direct-topic filters and receipt extraction;
5. raw client APIs remain available while generated methods use the single descriptor API;
6. all source fragments emitted in this release are safely escaped and all emitted identifiers are validated contextually;
7. curated and typed-contract fixtures compile and pass focused round-trip tests against the coordinated client release;
8. the real swap app removes runtime method-name lookup, handwritten return/abort decoding, fixed-length event repair, positional direct-topic filters, and handwritten receipt event scanning;
9. deferred binders, root-receipt decoders, event encoders, hashed-topic filters, naming migration, and decoder budgets are not implementation prerequisites.
