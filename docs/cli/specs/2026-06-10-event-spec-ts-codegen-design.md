# Event Specs in TS Codegen and SDK 0.15.0 Update

Status: approved design, pre-implementation.

## Goal

Adopt sdk_rs 0.15.0, which adds event definitions to the contract spec
(`ContractSpec.events`), and surface those events in the TypeScript
bindings emitted by `groundhog codegen ts` / `groundhog build
--bindings ts`. Update the `groundhog new` templates so a freshly
scaffolded counter contract demonstrates the full path: `#[derive(Event)]`
→ `emits(...)` declaration → spec → typed TS decoder.

## Background

sdk_rs 0.15.0 (commit `649ecee`) adds `ContractEventDef` to the spec
model: `name`, a 32-byte `signature` (topic 0), `topics` (the indexed
fields), and `fields` (the XDR-encoded payload). Events land in a
contract's spec only when declared — `emits(...)` on `#[contract(...)]`
or an interface trait, or forced via `#[event_spec(export)]` on the
derive. Topic values on the wire are SHA-256 hashes of the XDR-encoded
field value (raw passthrough only for `[u8; 32]`), so topic values are
not recoverable by a decoder; only payload fields decode to typed
values.

The protocol-ts packages already model `ContractSpec.events` and
`ContractEventDef`, and `ContractEvent`/`EmittedEvent` results from
`getEvents()` and receipts carry `topics: Uint8Array[]` plus
`data: Uint8Array`. The current CLI codegen emits a spec literal with no
`events` key, which stops type-checking against the updated
`@groundhogchain/contract-spec` — so the spec-literal change is required
for compatibility regardless of decoder scope.

## Changes

### 1. Dependency bumps (workspace `Cargo.toml`)

- `groundhog-xdr` `=0.13.7` → `=0.17.0` workspace-wide. The 0.15.0 spec
  crates pin groundhog-xdr 0.17.0, and the CLI passes
  `ContractEntrypointID` from `contract_entrypoint_id` into tx building,
  so a dual-version graph would clash at real type seams. Known fallout
  includes the `ContractBlob.wasm` → `.data` rename; fix compile errors
  across json-rpc-types, gateway, signing, and CLI as they surface. If
  schema docs shift, regenerate the OpenRPC snapshot with
  `UPDATE_SNAPSHOTS=1` and review the diff.
- `groundhog-contract-spec`, `groundhog-spec-tool`,
  `groundhog-contract-util` `0.14.7` → `0.15.0`.

### 2. TS codegen (`crates/tools/cli/src/codegen.rs`)

**Spec literal.** Emit `events: [...]` with each event's `name`,
`signature` bytes, `topics`, and `fields`, reusing the existing
field/type emitters.

**Per-event type + companion const.** For each spec event, emit a
same-named `interface` + `const` pair (TS declaration merging), so the
event name is both the decoded type and the value carrying the
signature and decoder:

```ts
export interface PingEvent {
  name: "PingEvent";
  topics: { tag: Uint8Array };
  fields: {};
}
export const PingEvent = {
  /** Topic-0 signature hash from the contract spec. */
  signature: fromHex("ab12…"),
  /** Returns null if topic 0 does not match `signature`. */
  decode(event: { topics: Uint8Array[]; data: Uint8Array }): PingEvent | null { … },
} as const;
```

- Topic fields are typed `Uint8Array` (hashed on the wire); payload
  fields use the existing `ts_type_ref` mapping.
- `decode` matches `event.topics[0]` against `signature`, maps topics
  1..n to named topic keys, and decodes payload fields in declaration
  order via `resolveCodec(field.ty, namedCodecs)` over an `XdrReader`.
  Both are already exported (`@groundhogchain/client`,
  `@groundhogchain/xdr`); no client-ts changes required.
- The structural `{ topics; data }` parameter matches `EmittedEvent`
  from both receipts and `getEvents()` pages.
- `null` means "not this event" (topic-0 mismatch). A matching
  signature with an undecodable payload throws, like the existing
  codec paths — that is corrupt data, not a different event.
- `signature` doubles as the `topic0` filter value for `getEvents`;
  no separate `eventSignatures` map.

**Combined decoder.** A per-contract union and dispatcher composed from
the per-event decoders, so consumers can decode mixed streams or pick
one event explicitly:

```ts
export type CounterEvent = PingEvent; // union over all events
export function decodeEvent(event: { topics: Uint8Array[]; data: Uint8Array }): CounterEvent | null;
```

Contracts with no events emit none of this section. Event names share
the generated file's top-level namespace with type-def names; rely on
spec-tool's name validation for collisions and surface a codegen error
if an event name collides with a generated type name.

### 3. Templates (`crates/tools/cli/templates/`)

- `Cargo.toml.tmpl`: `sdk = "0.14.11"` → `"0.15.0"`; update the
  `scaffold_test.rs` version assertion.
- Counter `implementation.rs.tmpl`: `#[contract(throws = CounterError)]`
  → `#[contract(throws = CounterError, emits(PingEvent))]`. Without the
  declaration, `PingEvent` silently disappears from the spec under
  0.15.0's declaration-based inclusion.
- Verify `rust-toolchain.toml.tmpl` (nightly-2026-01-30) still matches
  sdk_rs 0.15.0's toolchain; bump if sdk_rs moved.

### 4. `.spec.json` sidecar

Events appear automatically via spec-tool's updated
`render_spec_json`. Assert presence in an existing sidecar test rather
than adding new rendering code.

## Testing

- Codegen unit tests: a `ContractSpec` fixture with events (topic +
  payload fields, including a Named payload type) asserting the emitted
  interface/const pair, spec-literal `events` entry, and combined
  decoder; a no-events fixture asserting the section is absent.
- Scaffold integration test: SDK version assertion, `emits(...)`
  present in the generated counter.
- Workspace `cargo fmt --check`, `clippy --all-targets -- -D warnings`,
  `cargo test --workspace` after the xdr bump.
- If a hog-devnode/sdk toolchain is available: `groundhog build
  --bindings ts` on a scaffolded counter and confirm `PingEvent` lands
  in `.spec.json` and the bindings.

## Out of Scope

- client-ts / protocol-ts changes (event types and codec primitives
  already shipped there).
- TS topic-hash helpers (computing `hash_to_topic` values from typed
  inputs for filters) — needs sha256 + XDR-encode in TS; sensible
  client-ts follow-up.
- Gateway behavior changes; the spec format is not part of the JSON-RPC
  wire surface.
- The `ExportedEvent` fragment split (reserved in sdk_rs for a future
  opt-in path).
