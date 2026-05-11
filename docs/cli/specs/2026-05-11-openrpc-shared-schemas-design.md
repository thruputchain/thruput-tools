# OpenRPC Shared Schemas

Lift named types in the generated OpenRPC artifact into a single
top-level `components.schemas` registry, replacing per-method inline
schemas with `$ref`s. The artifact stops repeating the same type tree
across every method that uses it; downstream codegen (`client-ts`)
gains stable named types instead of inlined anonymous shapes.

## Goal

The generated artifact (`groundhog-open-rpc-spec`) is consumed by
`client-ts` via that crate's regen script. Today every method entry
embeds a full `schemars::schema_for!(T)` output, so a type used in N
methods appears N times. Codegen has no stable handle on the type and
must invent anonymous TS interfaces per use site. This design pivots
to the standard OpenRPC layout: one canonical schema per named type
under `components.schemas`, with `$ref` at every use site.

Scope: **every named Rust type** referenced by the RPC surface is
lifted, regardless of use count. Primitives (e.g. `u64`, `String`)
stay inline at use sites. Schemars decides what counts as "named" via
its existing `JsonSchema` machinery.

Out of scope for this work:

- Errors on methods. OpenRPC supports an `errors` array per method
  and a shared `components.errors` block; this design touches neither.
  Modeling RPC errors in the artifact is a separate design pass.
- Tags beyond the existing `tag = "..."` macro argument.
- OpenRPC `examples`, `servers`, `externalDocs`.

## Migration Posture

`client-ts` is the only known consumer today; the spec is published on
the private registry but no external SDKs depend on it. The artifact
wire format changes once, with no dual-emit shim.

The repo versions every workspace crate via `[workspace.package]
version` in `Cargo.toml`, and every workspace dependency hard-pins
`=<version>` with the explicit "must be kept in sync" comment. Per-
crate versioning does not exist. The migration is therefore a
**workspace-wide bump from `0.3.x` to `0.4.0`**, cascading to every
member crate — the same shape as commit `6908876`, where the workspace
moved to `0.3.1` for a change that only touched gateway + rpc-api +
rpc-server.

- Bump `[workspace.package] version` to `0.4.0` and update each
  `groundhog-* = { ..., version = "=0.4.0", ... }` entry in
  `[workspace.dependencies]` to match.
- `client-ts` regen runs against the new published spec.
- CHANGELOG entry records that this workspace bump carries an OpenRPC
  artifact wire-format change, even though the crate-version delta
  looks like a routine release.

## Implementation Approach

Thread a single `schemars::SchemaGenerator` through the project. Each
descriptor calls `generator.subschema_for::<T>()` instead of the
one-shot `schemars::schema_for!(T)` macro. Schemars itself produces
`$ref` for named types and inline schemas for primitives, accumulating
named definitions in the generator's internal map. At serialize time
the generator's definitions become `components.schemas`.

Schemars does the dedup and identity work; the generator does not
need a custom registry, post-processing pass, or structural-equality
check.

`SchemaSettings::definitions_path` is set to `#/components/schemas/`
so every `$ref` schemars emits already points at the OpenRPC location
— no path rewriting after the fact.

## Public API of `groundhog-open-rpc`

```rust
pub struct Project {
    info: ProjectInfo,
    modules: Vec<Module>,
    generator: SchemaGenerator,
}

impl Project {
    pub fn new(info: ProjectInfo) -> Self;

    /// Extend the project with a module. The closure receives a
    /// builder that shares the project's schema generator, so every
    /// named type used by the module's methods gets registered once
    /// in `components.schemas`.
    pub fn module(&mut self, f: impl FnOnce(&mut ModuleBuilder<'_>));

    pub fn to_json(&self) -> serde_json::Value;
}

pub struct ModuleBuilder<'a> {
    methods: Vec<MethodDoc>,
    generator: &'a mut SchemaGenerator,
}

impl ModuleBuilder<'_> {
    pub fn add_method(&mut self, doc: MethodDoc);
    pub fn generator(&mut self) -> &mut SchemaGenerator;
}

pub fn param_descriptor<T: JsonSchema>(
    gen: &mut SchemaGenerator,
    name: &str,
) -> ContentDescriptor;

pub fn result_descriptor<T: JsonSchema>(
    gen: &mut SchemaGenerator,
    name: &str,
) -> ContentDescriptor;
```

Breaking changes (intentional, the version bump catches them):

- `Module::new`, `Module::add_method`, and `Project::add_module` are
  removed. Modules are built only via `Project::module(|m| ...)`.
- `param_descriptor` / `result_descriptor` gain a `&mut SchemaGenerator`
  first argument.
- `ContentDescriptor.schema` may now be a `$ref` object instead of an
  inline schema. The Rust type is unchanged (`serde_json::Value`); the
  shape varies per type.

These breakages are acceptable because **`groundhog-open-rpc` is an
internal implementation detail**. The externally-visible consumer is
`client-ts` reading the generated JSON, not any Rust caller of the
crate. The only Rust callers are `groundhog-open-rpc-macros` (the
companion-struct generator) and `groundhog-open-rpc-spec` (the binary
that emits the artifact), both in this workspace. Both update
atomically with the API change.

Preserving the old `Module::new`/`add_method`/`add_module` surface
would require either a parallel set of descriptor helpers (legacy
inline vs new shared-registry — two ways to build descriptors with
different observable JSON output) or a single helper with a hidden
one-shot generator (produces inline schemas, contradicts the
shared-registry goal). Neither is worth the API-stability theatre for
a crate with no external Rust consumers.

## Macro Change

`#[open_rpc]` in `groundhog-open-rpc-macros` switches its generated
companion from "returns a `Module`" to "appends to a `ModuleBuilder`":

```rust
impl FooOpenRpc {
    pub fn module_doc(builder: &mut groundhog_open_rpc::ModuleBuilder<'_>) {
        let params = vec![
            groundhog_open_rpc::param_descriptor::<String>(builder.generator(), "tx_hex"),
        ];
        let result =
            groundhog_open_rpc::result_descriptor::<TxHash>(builder.generator(), "TxHash");
        builder.add_method(groundhog_open_rpc::MethodDoc {
            name: "groundhog_sendRawTransaction".to_owned(),
            params,
            result,
            ..
        });
        // one block per method
    }
}
```

The macro's user-facing attributes are unchanged. Every existing
`#[open_rpc(namespace = "...", tag = "...")]` annotation keeps working
without edit.

Caller side (`open-rpc-spec/main.rs`) changes from:

```rust
spec.add_module(GroundhogApiOpenRpc::module_doc());
```

to:

```rust
spec.module(|m| GroundhogApiOpenRpc::module_doc(m));
```

## Output Format

`Project::to_json()` produces:

```json
{
  "openrpc": "1.2.6",
  "info": { "title": "...", "version": "..." },
  "methods": [
    {
      "name": "groundhog_getTransactionReceipt",
      "params": [ /* ... */ ],
      "result": {
        "name": "TransactionReceipt",
        "schema": { "$ref": "#/components/schemas/TransactionReceipt" }
      }
    }
  ],
  "components": {
    "schemas": {
      "TransactionReceipt": { "type": "object", "properties": { /* ... */ } },
      "HexBytes": { "type": "string", "pattern": "^0x..." }
    }
  }
}
```

- Named types appear once in `components.schemas` under their
  schemars-assigned name (bare Rust type name by default).
- Use sites in `methods[].params[].schema` and
  `methods[].result.schema` carry `{"$ref": "..."}`.
- Primitives stay inline at use sites; they do not get
  `components.schemas` entries.

## Name-Collision Policy

The current RPC surface has no name collisions — every Rust type
referenced by the trait has a unique short identifier. The policy
exists for future scenarios where two distinct Rust types resolve to
the same `schemars::JsonSchema::schema_name()`.

**Policy: accept the global effect of `#[schemars(rename = "...")]`.**
Per `CLAUDE.md`'s schema-ownership rules, the JSON projection of XDR
types lives in `api-types`; renaming a type there changes its
JsonSchema name on every emit path (OpenRPC, `groundhog test --json`,
future codegen). When a collision arises, the resolver explicitly
considers and signs off on the global impact when choosing the
renamed identifier. The OpenRPC artifact does not maintain an
independent aliasing layer.

**Why not OpenRPC-local aliasing?** schemars 1.x `Transform`s can
rewrite schema values, but they do not rename the
`components.schemas` map keys created by `SchemaGenerator`, nor do
they rewrite every matching `$ref` string in the final artifact. An
OpenRPC-local aliasing layer therefore means a real post-processing
pass (or custom registry logic) over the generated JSON. That code is
not justified until the codebase has an actual collision. Per
`CLAUDE.md`'s "delete unused code rather than preserve it" guidance,
we do not build it pre-emptively.

**What schemars does on collision.** Under one `SchemaGenerator`,
schemars 1.x auto-suffixes duplicate schema names (`Foo`, `Foo2`,
...) rather than panicking or overwriting. That behavior is accepted
as an implementation detail, not as a desired public contract. The
current RPC surface has no collisions, so v1 relies on snapshot review
to surface an unexpected `Foo2`-style key. If a collision is
introduced, resolve it deliberately before publish: prefer global
`#[schemars(rename = "...")]`, or add explicit post-processing /
validation if a global rename is unacceptable.

## Testing

Three layers:

**Unit tests in `crates/open-rpc/src/lib.rs`**

- Existing tests migrate to the new `Project::module(|m| ...)` API.
- Shared-schema dedup: two methods using the same named type produce
  one `components.schemas` entry plus two `$ref` use sites.
- Primitive inline: a method returning `u64` keeps `{"type": "integer"}`
  inline, with no `components.schemas` entry created for the primitive.
- Collision rename: two structs named `Foo` from different modules,
  disambiguated via `#[schemars(rename = "...")]`, both appear in
  `components.schemas` under their renamed identifiers.

**Integration test in `crates/open-rpc-spec/`**

Two complementary checks:

- **Canonical artifact snapshot.** Check the generated spec JSON into
  the repo (e.g. `crates/open-rpc-spec/snapshots/groundhog-spec.json`).
  The integration test runs `build_spec()` and asserts byte-equal
  match against the committed file. An `UPDATE_SNAPSHOTS=1` env var
  regenerates the snapshot when output changes intentionally. The
  diff during a workspace bump or macro edit is the review surface
  that catches unexpected schema hoisting, renamed component keys,
  nested `$ref` churn, or any other artifact-shape regression — every
  schemars or macro change is intentionally reviewed against the
  full generated artifact, not a handful of structural assertions.
- **Structural invariants.** Alongside the snapshot, assert
  invariants that document intent and produce clearer failures than
  a thousand-line diff: a representative reused type appears exactly
  once under `components.schemas`; every method that references it
  uses `$ref`; `components.schemas` is non-empty; the artifact parses
  as valid JSON. The snapshot catches regressions; the invariants
  document the contract.

This combination is the right shape for a generated downstream
contract: brittleness against schemars version bumps becomes a
feature — schemars is workspace-pinned, so any change to its output
already requires an explicit, reviewed Cargo.toml edit, and that PR
should regenerate and re-review the snapshot in the same change.

**Macro test in `crates/open-rpc-macros/tests/`**

- `#[open_rpc]` applied to a synthetic trait expands to code that uses
  the new `ModuleBuilder` API and compiles. `trybuild` or a plain
  compile test. The macro test does not verify schema content — that
  is the unit tests' job.

**Deliberately out of scope**

- OpenRPC 1.2.6 schema-validation of the artifact (no production-grade
  Rust validator; not worth a JS dependency in CI for one check).

## Risks

- **Schemars `subschema_for` behavior in 1.x.** The design assumes
  schemars 1.x's `SchemaGenerator::subschema_for::<T>()` produces a
  `$ref` for `JsonSchema`-implementing structs/enums and inlines
  primitives. Confirm in implementation. If the cut-off is different
  from "anything with a `JsonSchema` name," the integration test will
  surface it.
- **`definitions_path` override actually changes emitted `$ref` paths.**
  Confirm `SchemaSettings { definitions_path: "#/components/schemas/" }`
  takes effect for nested refs, not just the top level.
- **Macro hygiene.** The new `module_doc(builder: &mut ModuleBuilder)`
  signature must work whether the caller imports `groundhog_open_rpc`
  under its full path or via a re-export. Test with a re-export to
  catch path issues.
