# OpenRPC Wire-Format Version Decoupling

**Date:** 2026-05-11
**Status:** Shipped. The current code declares
`groundhog_rpc_api::WIRE_FORMAT_VERSION`; as of 2026-05-23 it is `2.0.0`.
This document preserves the design rationale and original rollout plan.

## Context

The canonical OpenRPC artifact at
`crates/json-rpc/open-rpc-spec/snapshots/groundhog-spec.json` is the external
contract SDK authors pin on. Its `info.version` currently mirrors the
Rust workspace version via `env!("CARGO_PKG_VERSION")`, so any workspace
patch bump (`0.3.1 → 0.3.2`) forces a new spec snapshot even when the
wire shape hasn't changed. Conversely, a real wire-format change today
inherits whatever workspace bump happens to land.

The cross-repo release model in `~/code/gh/release` pins component git
refs by tag in `release-manifest.toml`. A platform release can carry a
new `groundhog-rpc` tag for bugfix reasons that don't touch the wire
contract. SDK consumers shouldn't have to re-pin in that case.

## Goal

A single SemVer signal that SDK authors can pin on, bumped only when
the wire contract changes:

- consumer pins `wireFormat == 1.x.x`, re-evaluates only on `2.0.0`
- platform release bumps `groundhog-rpc` for bugfixes without forcing
  spec re-pin
- runtime nodes report their wire-format version alongside other
  version dimensions

## Prior art

Surveyed before settling on the shape:

- **Ethereum execution-apis** — independent SemVer on `info.version`,
  bumped only on wire changes. Major = breaking, minor = additive,
  patch = doc/tooling. Decoupled from any client release. This is the
  precedent SDK authors are most likely to recognize.
- **Sui** — `info.version` tracks node release; separate runtime
  `protocolVersion` integer bumps independently.
- **Substrate JSON-RPC v2** — method-group versioning in names
  (`chainHead_v1_follow`); v1 and v2 coexist as separate groups.
  Heaviest but most flexible. Overkill at our stage.
- **Stellar Soroban RPC** — no OpenRPC, but `getVersionInfo` returns
  `version` + `captiveCoreVersion` + `protocolVersion` (three
  dimensions). Familiar shape for SDK authors from that ecosystem.

Pattern adopted: Ethereum execution-apis style for the spec; Stellar
`getVersionInfo` style for the runtime method.

## Design

Two changes, shippable as separate PRs:

### Part A: independent spec SemVer

`info.version` becomes a hand-maintained constant in **`rpc-api`**,
decoupled from `CARGO_PKG_VERSION`:

```rust
// crates/json-rpc/rpc-api/src/lib.rs

/// Wire-format SemVer for the JSON-RPC surface.
///
/// Bump rules (see CLAUDE.md → "OpenRPC Wire Version"):
/// - major: breaking change to method shape, params, results, or errors
/// - minor: additive change (new method, new optional field, new error)
/// - patch: documentation / description / example refinement only
pub const WIRE_FORMAT_VERSION: &str = "2.0.0";
```

`open-rpc-spec::build_spec()` reads
`groundhog_rpc_api::WIRE_FORMAT_VERSION` for `info.version`. `rpc-api`
is also the home consumed by `rpc-server` for the `groundhog_version`
RPC in Part C — single source of truth, both consumers already depend
on `rpc-api`.

**Current value:** `2.0.0`. Future bumps are intentional PR-author decisions,
not automatic.

**Enforcement:** the existing snapshot test
(`crates/json-rpc/open-rpc-spec/tests/spec_snapshot.rs`) already pins the
artifact byte-for-byte. Any change to the spec forces a snapshot
update; reviewers verify the version bump in the same PR matches the
nature of the change. No additional CI gate — bump discipline is part
of code review.

**Documentation:** CLAUDE.md gains a short "OpenRPC Wire Version"
section under Gateway Conventions describing the bump rules and
pointing at the constant.

The Rust workspace version stays orthogonal — it identifies the build
that produced the spec, not the wire contract. The two versions can
diverge freely.

### Part C: `groundhog_version` RPC

A read-side RPC method exposing implementation and wire-format version
information at runtime. Mirrors Stellar `getVersionInfo` in shape.

**Method:** `groundhog_version`
**Params:** none
**Result:**

```rust
pub struct VersionInfo {
    /// Wire-format SemVer of the OpenRPC spec this node implements.
    pub wire_format: String,
    /// Implementation version (Rust workspace version).
    pub implementation: String,
    /// Git commit hash this node was built from (best-effort).
    pub commit_hash: Option<String>,
}
```

JSON shape (camelCase per convention):

```json
{
  "wireFormat": "1.0.0",
  "implementation": "0.3.1",
  "commitHash": "7484125..."
}
```

`protocolVersion` is **not** included today — the chain doesn't expose
one yet. The struct is `#[non_exhaustive]` so adding it later is
additive (minor bump).

**Sourcing the values:**

- `wireFormat` — `groundhog_rpc_api::WIRE_FORMAT_VERSION` (see "Where
  the constant lives" below).
- `implementation` — injected per-binary via a `BuildInfo` struct
  passed to the `rpc-server` constructor. Each binary populates it
  from `env!("CARGO_PKG_VERSION")` of its own crate
  (`groundhog-gateway` or `groundhog-node-supervisor`). Using a pure
  `env!()` inside `rpc-server` would report the `groundhog-rpc-server`
  crate version, which is misleading when binaries diverge later.
- `commitHash` — same `BuildInfo` struct, populated via `vergen` or a
  `GIT_COMMIT` env var by each binary. `None` if the build had no git
  context. Not derivable inside a library crate.

`BuildInfo` is a tiny struct (`{ implementation_version, commit_hash }`)
threaded through the existing `rpc-server` server-construction path. No
runtime fetching — both fields are `&'static str` resolved at the
binary's build time.

**Where the wire-format constant lives:**

`WIRE_FORMAT_VERSION` is declared in **`rpc-api`**, not in
`open-rpc-spec`. Rationale: `rpc-api` already owns the JSON-RPC method
surface — the thing being versioned. Both `open-rpc-spec` (consumes it
for `info.version`) and `rpc-server` (consumes it for
`groundhog_version`) already depend on `rpc-api`. Putting the constant
in `open-rpc-spec` would force `rpc-server` to depend on the spec-
generator crate just to read a `&'static str`.

**Where the RPC method lives:**

- `api-types::version::VersionInfo` — wire type (`#[non_exhaustive]`,
  camelCase).
- `rpc-api` — `fn version(&self) -> RpcResult<VersionInfo>` on
  `GroundhogApi` trait. Also re-exports `WIRE_FORMAT_VERSION`.
- `rpc-server` — pure implementation reading
  `rpc_api::WIRE_FORMAT_VERSION` for `wireFormat` and the injected
  `BuildInfo` for `implementation` / `commitHash`. No backend trait
  dispatch.
- Gateway and node-supervisor each construct the server with their own
  `BuildInfo` and inherit `version` through their
  `GroundhogApiServer` impl.

**Field semantics consumers can rely on:**

- `wireFormat` — the **only** field SDKs may branch on. SemVer
  contract per Part A.
- `implementation` — diagnostic. May be the same workspace version
  today across binaries; consumers must not branch on it.
- `commitHash` — diagnostic. May be absent.

### Part B (rejected): runtime cross-repo manifest integration

Considered surfacing `wireFormatVersion` in `release-manifest.toml`
alongside component git refs. Rejected: the spec is the source of
truth and is already pinned by the component git ref. Duplicating the
version in the manifest creates a sync problem with no upside —
tooling that wants "what wire version does platform v0.2.0 expose?"
reads it out of the spec snapshot at the pinned ref, or queries
`groundhog_version` against a running node.

`release-manifest.toml` stays narrow: it pins git refs, not wire
versions.

## Out of scope

- **Method-name versioning** (Substrate v1/v2 style) — would let v1
  and v2 of `getEvents` coexist. Heavy mechanism; revisit only when
  we have a concrete migration that benefits from coexistence.
- **`protocolVersion` from hogd** — needs hogd-side work to expose
  the chain's protocol version. Add to `VersionInfo` when available.
- **Publishing the spec to a stable standalone URL** — Ethereum has
  `ethereum.github.io/execution-apis/`. We can do this once we have
  external SDK consumers. Today the snapshot lives in-repo, which is
  sufficient for codegen consumers like client-ts.
- **CI gate enforcing version bump on snapshot change** — could add
  a check that "snapshot changed ⇒ `WIRE_FORMAT_VERSION` changed,"
  but reviewer enforcement is the lower-cost first step.

## Original Sequencing

This was the rollout plan when the design was written:

- **PR A: independent spec SemVer.** Add `WIRE_FORMAT_VERSION` to
  `rpc-api`, wire `open-rpc-spec::build_spec()` through it, update
  CLAUDE.md with the bump rules, regenerate the snapshot. Small,
  self-contained. Ships `1.0.0` — the existing artifact has been
  through review and is the stable baseline.
- **PR C: `groundhog_version` RPC.** New wire type
  (`api-types::version::VersionInfo`), new trait method on
  `GroundhogApi`, `BuildInfo` plumbing into the `rpc-server`
  constructor, gateway and node-supervisor populate `BuildInfo` from
  their own `env!()` and `vergen`. Adding a method is additive, so
  this PR **bumps `WIRE_FORMAT_VERSION` from `1.0.0` to `1.1.0`** in
  the same PR. That's the first real exercise of the bump rules.

## Testing

- Snapshot test continues to pin the artifact byte-for-byte. Any
  change requires `UPDATE_SNAPSHOTS=1` regeneration.
- `groundhog_version` gets a roundtrip integration test in the
  `client` crate against a test server, asserting the JSON field
  names and types (not specific values).
- Unit test in `open-rpc-spec` confirms `WIRE_FORMAT_VERSION` parses
  as valid SemVer.

## Risk / mitigation

- **Risk:** reviewer forgets to bump version when snapshot changes.
  **Mitigation:** snapshot diff in PR review makes the change visible;
  bump rules in CLAUDE.md make the obligation explicit. Promote to a
  CI gate later if drift becomes a problem.
- **Risk:** `implementation` field in `groundhog_version` leaks
  workspace version to clients who shouldn't depend on it.
  **Mitigation:** field doc and CLAUDE.md note that clients **must
  not** branch on `implementation`; it's informational. SDKs branch
  on `wireFormat` only.
