# Gateway Benchmark Bootstrap Plan

**Date:** 2026-05-21
**Status:** Partially implemented. Runtime-manifest readiness, gateway
consumption, benchmark consumption, and CLI manifest-first dev-account
discovery have landed; dev RPC removal, bootstrap-manifest input, and the
full-stack benchnet harness remain open.

## Goal

Make gateway benchmarking and local CLI workflows easy to run against realistic
backend state without turning `groundhog node` into a production-stack
launcher.

The benchmark path should be:

```text
groundhog-bench / loadgen
        |
        v
groundhog-rpc gateway
        |
        v
hogd-dev or full hogd/chuckd backend stack
```

The shared problem is bootstrap, not JSON-RPC service discovery alone. A useful
benchmark stack needs genesis, the dev-wallet contract, funded accounts, wallet
addresses, storage-cost settings, protocol identity, and backend endpoints.

The first implementation target is `hogd-dev`: it is the fastest place to
prototype the manifest producer and validate the CLI/bench/gateway consumers.
The contract should not be hogd-dev-specific. The same bootstrap/runtime shapes
must also work for a production-style backend made of discrete hogd processes
and a real chuckd coordinator behind the real `groundhog-rpc` gateway.

## Current Pieces

Most of the pieces already exist:

- `hogd-dev` can bootstrap deterministic dev accounts and advertise them in
  readiness.
- `groundhog node` validates advertised dev-account metadata against local
  deterministic dev-key settings.
- `groundhog-rpc` already runs the production gateway path and connects to
  scheduler, indexer, DbGateway, and ViewGateway endpoints.
- `groundhog-bench` already provides an open-loop HTTP JSON-RPC load generator.
- The CLI dev-key flow already treats `dev-N` wallet addresses as backend
  metadata, not locally-derived addresses. It reads configured runtime
  manifests for wallet-address discovery.

The first runtime-manifest handoff now exists and is consumed by `groundhog
node`, `groundhog-rpc --runtime-manifest`, and `groundhog-bench
--runtime-manifest`. Remaining connective tissue is the cleanup and expansion
around that contract: the temporary dev-account RPC should be removed after the
manifest-first CLI migration proves out, bootstrap input should become
portable, and a full-stack benchmark harness should consume the same shapes.

Today's public JSON-RPC surface already covers most normal transaction and
state reads:

- `groundhog_sendRawTransaction`
- `groundhog_chainTip`
- `groundhog_getTransactionReceipt`
- `groundhog_getTransactionStatus`
- `groundhog_getEvents`
- `groundhog_getStorageAt` when DbGateway is configured
- `groundhog_view` when ViewGateway is configured
- `groundhog_version`

Some public methods exist but are still not wired in the gateway:

- `groundhog_getBlockByNumber`
- `groundhog_pendingTransactionCount`

The manifest should focus on what public JSON-RPC does not carry today:
backend endpoint topology, bootstrap account/wallet metadata, derivation
scheme, genesis hash, protocol identity, dev-wallet artifact identity, and the
storage cost model.

## Design Split

Use two related but distinct manifest shapes.

### Bootstrap Manifest

Input to a backend launcher. Describes desired genesis/bootstrap state.

```json
{
  "schemaVersion": 1,
  "kind": "groundhog-bootstrap",
  "networkId": "groundhog-bench-local",
  "profile": "local-dev",
  "protocol": "hogp/...",
  "devWallet": {
    "artifactHint": "./dev-wallet.wasm",
    "contentHash": "0x...",
    "deployEntrypoint": "0xadac498222148e4f",
    "doActionEntrypoint": "0x..."
  },
  "bootstrapAccounts": {
    "count": 100,
    "seed": "groundhog-bench-accounts",
    "initialStorageTokens": "1000000000"
  },
  "storageCostModel": {
    "profile": "hogd-default"
  }
}
```

This is intent. It should be small and deterministic. It may refer to artifacts
by a resolution hint plus a content hash; it should not be required to inline
large WASM blobs. The hint helps humans and launchers find the artifact, while
the hash verifies that the backend bootstrapped the intended bytes.

The bootstrap manifest is canonical input. Convenience CLI flags may lower into
this shape, but a flag that conflicts with an explicit manifest field should be
an error rather than a silent override.

For benchmark profiles, `protocol` should be pinned. The backend should reject
startup if the running binary cannot materialize that protocol version. Local
developer shorthand profiles may omit it and accept the binary's embedded
protocol, but reproducible benchmark inputs should not.

Where the bootstrap manifest names a profile, the runtime manifest expands it
into the concrete parameters actually applied. For example,
`storageCostModel.profile = "hogd-default"` lowers to explicit cost-model
fields in the runtime manifest.

For v1, `bootstrapAccounts` describes a homogeneous account group. All
bootstrapped accounts share the same derivation scheme and initial storage-token
funding. Account `role` values are producer-assigned runtime hints derived from
the selected profile, not input fields in the bootstrap manifest. Per-account
roles or per-role funding can be added later with an explicit account-spec
shape.

### Runtime Manifest

Output from a running backend. The runtime manifest is written to a file; the
readiness stdout record points at that file.

Readiness stdout remains a small, bounded lifecycle protocol:

```json
{
  "schemaVersion": 1,
  "status": "ready",
  "manifestPath": "/tmp/hogd-dev-xyz/runtime-manifest.json",
  "manifestHash": "0x..."
}
```

The runtime manifest file contains the full payload:

```json
{
  "schemaVersion": 1,
  "kind": "hogd-dev-runtime",
  "networkId": "groundhog-local",
  "protocol": "hogp/...",
  "genesisHash": "0x...",
  "producer": {
    "binary": "hogd-dev",
    "version": "...",
    "commitHash": "..."
  },
  "endpoints": {
    "scheduler": "127.0.0.1:12345",
    "indexer": "127.0.0.1:12346",
    "dbGateway": "127.0.0.1:12347",
    "viewGateway": "127.0.0.1:12348"
  },
  "bootstrapAccountDerivation": {
    "scheme": "groundhog-bootstrap-account-v1",
    "seed": "groundhog-dev-accounts"
  },
  "bootstrapAccounts": [
    {
      "index": 0,
      "role": "local_user",
      "publicKey": "0x...",
      "walletAddress": "0x...",
      "initialStorageTokens": "1000000000"
    }
  ],
  "bootstrap": {
    "profile": "local-dev",
    "devWallet": {
      "contentHash": "0x...",
      "deployedCodeHash": "0x...",
      "artifactHint": "./dev-wallet.wasm",
      "deployEntrypoint": "0xadac498222148e4f",
      "doActionEntrypoint": "0x..."
    },
    "storageCostModel": {
      "hash": "0x...",
      "baseDataItemCost": "12000",
      "bytestringCostPerByte": "10",
      "baseContractDeploymentCost": "12000",
      "contractDeploymentCostPerByte": "10",
      "authPerKeyCost": "1111",
      "rcqCostPerEntry": "8",
      "replayCacheCostPerEntry": "7",
      "balanceCost": "1234"
    }
  }
}
```

This is observed state. It answers: "What is actually running, where are the
services, which accounts are funded, and which wallet addresses should clients
invoke?"

The runtime manifest replaces the previous ad hoc readiness JSON without
changing the tiny one-line stdout contract: read exactly one bounded JSON line
from stdout, require `status = "ready"`, and load the manifest from
`manifestPath`.

The manifest file should be written in an owner-only directory with restrictive
permissions, and the readiness pointer includes `manifestHash` so consumers can
detect local file replacement between reading stdout and opening the manifest.
`manifestHash` is a hash of the exact manifest file bytes as written, not a
canonical JSON digest. Consumers should read the file bytes, verify the hash,
then parse the JSON.

If `hogd-dev` or a full-stack harness also grows a small admin HTTP server for
`/health` and Prometheus `/metrics`, it may expose `GET /manifest` as a
secondary runtime access path for the same manifest bytes. That endpoint should
not replace the startup readiness pointer: supervisors still need a bounded
local handoff that works before HTTP clients are configured. `GET /manifest`
is useful for remote bench workers and humans after startup, especially when
they cannot read the local manifest file.

Because the runtime manifest can include derivation seed material for local
dev/bench accounts, `GET /manifest` is an admin surface, not public chain RPC.
It should be loopback-only or otherwise operator-protected by default. If an
unauthenticated health server is required, expose only a redacted status
payload there and keep the full manifest on a protected endpoint. The response
should be immutable for a given backend run and should advertise the same
`manifestHash` used by readiness, for example through an `ETag` or explicit
field.

Optional endpoints should be absent when disabled, not present with `null`.
Consumers can treat endpoint presence as feature enablement.

Endpoint values in the runtime manifest are advertised endpoints: the addresses
consumers should dial. Bind addresses are operator-side process configuration
and should not appear in the manifest unless they are also the reachable
advertised address. Loopback runs can advertise `127.0.0.1`; cross-host runs
should advertise a reachable hostname or IP.

Any integer field that can plausibly exceed JavaScript's safe integer range
must be encoded as a decimal string. The examples use strings for storage-token
and cost-model quantities for that reason.

`schemaVersion` is an integer. Incompatible manifest shape changes bump it;
additive optional fields do not. Consumers should reject unknown future major
integers unless they explicitly opt into best-effort parsing.

The runtime manifest is not an alternate genesis encoding. It is observed output
from a running backend. Do not feed it back into backend launchers as bootstrap
intent; use the bootstrap manifest for that.

Runtime `bootstrap.devWallet.contentHash` repeats the artifact byte hash from
the bootstrap manifest. `deployedCodeHash` is the hash of the code actually
installed in genesis and may differ if the backend transforms the artifact
before deployment.

Terminology note: the runtime manifest uses `bootstrapAccounts` for
accounts/wallets materialized at bootstrap so local CLI, bench, and test
harnesses can submit valid transactions.

For v1, all `bootstrapAccounts` share the top-level
`bootstrapAccountDerivation` scheme and seed; `index` is the only key
discriminator. This matches the current deterministic account model and avoids
per-account derivation ambiguity. The array length is authoritative; do not
duplicate it in the derivation block.

`role` is a free-form producer/consumer hint, not a closed protocol enum.
Consumers must not assume a role exists unless their selected bootstrap profile
documents it.

`storageCostModel.hash` is a convenience fingerprint for comparing concrete
cost models across runs. It should be computed over a deterministic,
version-tagged encoding of the concrete cost-model fields, not over arbitrary
pretty-printed JSON. The full cost model remains authoritative.

## Ownership

### hogd / hogd-dev

- Owns genesis materialization.
- Owns dev-wallet deployment/bootstrap.
- Owns storage-token funding.
- Owns wallet address computation and advertisement.
- Emits the runtime manifest once all advertised endpoints and accounts are
  usable.
- May serve the same runtime manifest from an admin HTTP endpoint after startup,
  but does not treat that endpoint as the primary readiness contract.
- Computes and emits `genesisHash` from the complete bootstrap input actually
  materialized.
- Emits producer identity so benchmark runs can be tied back to the hogd binary
  and commit that created the runtime state.
- Prototypes the manifest producer in `hogd-dev` first, but keeps the schema
  suitable for a discrete-process hogd/chuckd backend.

### Full-Stack Benchmark Harness

- Starts full hogd and optionally chuckd/coordinator using the same bootstrap
  manifest concept.
- Emits the same runtime manifest shape, extended with coordinator endpoints
  when present.
- May write derived `groundhog-rpc` config as a convenience for hand inspection,
  but the runtime manifest is the source of truth for dev/bench orchestration.
- May launch either separate binaries/processes for each hogd component or a
  stitched binary with role-selecting flags. That process topology is an
  implementation choice; the manifest contract should not depend on it.

### groundhog-rpc Gateway

- Remains the production JSON-RPC process under benchmark.
- Consumes endpoint config, not genesis intent.
- Should not synthesize wallet addresses, balances, or genesis state.
- Should not expose `groundhog_dev_*` for the benchmark path. Bootstrap account
  metadata is orchestration data and should be consumed from the runtime
  manifest out-of-band.
- Continues to expose normal chain and state facts through public JSON-RPC.
  Manifest-only metadata should not be promoted into the production RPC surface
  unless real clients need it as chain data rather than orchestration data.
  `genesisHash` is the strongest candidate for promotion because it is a chain
  fingerprint; if promoted, prefer adding it to `groundhog_version` rather than
  minting a manifest-shaped RPC method.
- Accepts `--runtime-manifest <path>` for dev/bench runs and derives its backend
  endpoint config directly from the manifest. TOML remains the normal production
  config format when no manifest exists.

### groundhog-bench

- Consumes the runtime manifest or a derived bench config.
- Builds valid transaction corpora from advertised bootstrap accounts and wallet
  addresses.
- Derives local-only dev private keys from the advertised derivation scheme,
  seed, and index rather than reading private keys from the manifest.
- Targets the gateway HTTP JSON-RPC endpoint, not hogd-dev directly, when the
  goal is gateway benchmarking.

### Schema Ownership

The manifest Rust types and JSON Schema should live in `crates/api-types`.
Rust consumers can share the typed model, while `schemars` can publish a JSON
Schema artifact for TypeScript, Python, shell tooling, and C++ producer tests.
Hogd may produce the JSON from C++ without importing Rust code, but it should
test against the generated schema.

The removed `api-types::dev::DevAccount` local-node RPC type included
private-key material. The runtime manifest deliberately uses a different shape
with public bootstrap metadata only.

The removed `groundhog_dev_getAccounts` method was a local-node compatibility
surface, not the long-term discovery contract. CLI `dev-N` wallet-address
resolution and `groundhog-bench` now derive account metadata from the runtime
manifest.

## Implementation Status

As of 2026-05-22:

- [x] `hogd-dev` readiness pointer to a versioned runtime manifest.
- [x] `groundhog node` reports `data.runtime_manifest.path` and hash in
  startup JSON.
- [x] Supervisor parses the runtime manifest and validates advertised
  dev-account metadata against configured deterministic dev keys.
- [x] `groundhog-bench --runtime-manifest` generates valid dev-wallet
  `sendRawTransaction` workloads from `bootstrapAccounts`.
- [x] `groundhog-rpc --runtime-manifest` overlays gateway backend endpoints
  from the manifest.
- [x] Smoke and repeatable local benchmark scenarios cover the manifest-backed
  CLI-node and standalone-gateway paths.
- [x] CLI `dev-N` wallet-address resolution reads a configured runtime
  manifest.
- [x] Remove the dev-account RPC compatibility path.
- [ ] Define and consume a portable bootstrap manifest input for `hogd-dev`.
- [ ] Build the full-stack benchnet harness for hogd/chuckd topologies.

## Phased Plan

### Phase 1: Runtime Manifest Readiness

- [x] Replace `hogd-dev` readiness with a small versioned readiness pointer.
- [x] Write the versioned runtime manifest to disk.
- [x] Update `node-supervisor` parsing to accept the new manifest shape.
- [x] Preserve the one-line stdout contract and bounded read for the pointer.
- [x] Keep stderr for logs.
- [ ] If a local admin HTTP server exists, optionally expose `GET /manifest` as a
  protected post-startup retrieval path for the same manifest.
- [x] Keep endpoint aliases only if needed during the transition; prefer canonical
  manifest keys going forward.
- [x] Do not write private keys into the runtime manifest; local dev signers
  remain deterministic and are derived in process when needed.

Deliverable: `groundhog node` still works, but its startup data is the runtime
manifest rather than ad hoc readiness JSON.

### Phase 2: Benchmark Consumption

- [x] Teach `groundhog-bench` to read a runtime manifest.
- [x] Generate valid dev-wallet transaction workloads from `bootstrapAccounts`.
- [x] Keep the current URL-only mode for simple RPC smoke tests.
- [ ] Add machine-readable benchmark output if missing for comparison across runs.
- [x] Teach CLI `dev-N` wallet-address resolution to use a configured runtime
  manifest.

Deliverable: benchmark submit workloads are valid against a bootstrapped
backend instead of relying on local assumptions. The benchmark and CLI
dev-account discovery portions are done.

### Phase 2.5: Remove Dev RPC Compatibility

- [x] Once the manifest-first migration has proven out, remove the dev-account
  RPC compatibility path rather than letting it linger.
- [x] Delete `GroundhogDevApi` / `groundhog_dev_getAccounts` from the generated API
  surface and OpenRPC spec unless another concrete local-only use remains.
- [x] Remove supervisor `dev_accounts` plumbing that exists only to serve that RPC.
- [x] Keep deterministic dev-key signing support; only the account-discovery
  surface moves to the runtime manifest.

Deliverable: bootstrap account discovery has one maintained path: the runtime
manifest.

### Phase 3: Gateway Config Generation

- [x] Add `groundhog-rpc --runtime-manifest <path>` so dev/bench runs can consume
  advertised backend endpoints directly.
- [x] Keep TOML config for production deployments and for hand-authored configs
  where no runtime manifest exists.
- [x] Keep benchmark runs on the production gateway binary.

Deliverable: the gateway can start against a bootstrapped backend without
hand-copying ports or generating an intermediate config file. This phase is
implemented for the hogd-dev runtime-manifest path.

### Phase 4: Bootstrap Manifest Input

- Define the bootstrap manifest accepted by `hogd-dev`.
- Move dev account count, seed, token amount, dev-wallet profile, and cost-model
  profile into that manifest.
- Keep simple CLI flags as convenience shorthands that lower into the manifest.
- Leave arbitrary `initialContracts` out of this phase. Add it in a later phase
  only when there is a concrete entry shape and producer implementation.

Deliverable: local dev and benchmark backends can request the same genesis
shape in a portable way.

### Phase 5: Full-Stack Benchnet Harness

- Build a non-product harness that starts full hogd and optionally chuckd from a
  bootstrap manifest.
- Emit the same runtime manifest as `hogd-dev`.
- Use `submission_backend = hogd_scheduler` or `chuckd_coordinator` in the
  gateway's manifest-derived config depending on the selected topology.
- Keep the harness compatible with both possible hogd deployment styles:
  separate component binaries/processes, or one stitched binary running a
  selected role via flags.

Deliverable: gateway benchmarking can compare direct-hogd and chuckd-routed
submission paths without changing `groundhog node` semantics.

Phase dependency: Phase 1 is the keystone. Phases 2 and 3 can proceed in
parallel once the runtime manifest exists. Phase 4 must precede Phase 5 because
the full-stack benchnet needs a portable bootstrap input.

## Non-Goals

- Do not make `groundhog node` a production full-node launcher.
- Do not make the production gateway responsible for genesis/bootstrap.
- Do not inline large genesis artifacts in one-line readiness output.
- Do not expose dev-account private keys or dev metadata from production
  gateway mode.
- Do not keep `groundhog_dev_*` as the long-term bootstrap account discovery
  surface once manifest consumers are in place.
- Do not make the manifest contract depend on whether hogd components run as
  separate processes or as one stitched binary with role flags.
- Do not treat the runtime manifest as the canonical genesis source of truth;
  it is observed output.
- Do not treat the runtime manifest as an alternate bootstrap input format.

## Open Questions

1. What exact dev-wallet artifact resolution hints should the bootstrap
   manifest support: bundled artifact name, filesystem path, protocol-owned
   profile name, or more than one?
2. Should any manifest-only fields beyond `genesisHash` graduate into public
   JSON-RPC, and if so which are true chain facts rather than orchestration
   metadata?
3. Should the full-stack benchnet prefer discrete hogd component processes or a
   stitched binary with role-selecting flags?

## Review Position

The scope appears manageable because the codebase already has:

- backend plane traits and a production gateway composition;
- hogd-dev process supervision and readiness parsing;
- deterministic dev-key derivation;
- backend-advertised wallet address resolution;
- runtime-manifest account discovery for CLI and benchmark workloads;
- an initial open-loop benchmark harness.

The main design work is to promote the existing readiness and bootstrap-account
metadata into a versioned runtime manifest, then use that artifact to drive
gateway benchmarking without changing the meaning of `groundhog node`.
