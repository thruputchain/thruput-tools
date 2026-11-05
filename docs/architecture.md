# System architecture

This workspace contains Thruput's off-chain tools and public JSON-RPC interface.
Contract developers use the CLI to build, test, and deploy contracts. Applications
use RPC clients or generated contract bindings to call them.

The `hogd` repository owns execution and chain state. The SDK owns contract APIs,
guest startup, and linker scripts. The protocol repositories define shared data
types in XDR and provide language bindings. This workspace uses `thruput-xdr`.

## From application to runtime

```mermaid
flowchart LR
    C[CLI / application / test harness] -->|JSON-RPC| G[Standalone gateway]
    C -->|JSON-RPC| L[Local node supervisor]
    G -->|x23| H[Managed hogd services]
    L -->|x23| D[Owned hog-devnode process]
```

The gateway connects to separately managed backend services. The local supervisor
starts a `hog-devnode` process and connects to the services it advertises. Both
use the same public server and backend response mapping. Local integration tests
exercise that API against the real runtime.

x23 is hogd's internal RPC transport. Applications use JSON-RPC.

The public server implements RPC methods against backend traits. Shared adapters
translate hogd results. The gateway owns its connections, admission limits,
deadlines, and recovery. The supervisor also owns child startup, readiness, logs,
and shutdown. A [runtime manifest](gateway/runtime-manifest.md) carries the local
service endpoints and network identity between tools.

Backend services divide the work: the scheduler admits transactions, the indexer
serves committed history, the database gateway reads state, and the view gateway
simulates calls. Coordinator control is used for local block production.
Execution and transaction validity remain in hogd; public traffic limits belong
to the gateway.

See the generated [JSON-RPC reference](../crates/json-rpc/open-rpc-spec/snapshots/thruput-spec.md)
for method behavior and encodings. The wire version is independent of the tools'
release version; `getVersionInfo` reports the endpoint's version and methods.

## From contract source to invocation

The CLI creates a Rust project and builds it with the project's pinned toolchain.
The RV32 lowerer converts the ELF into a contract artifact. The build embeds the
canonical contract spec outside guest memory and exports JSON/XDR sidecars.
Invocation and TypeScript bindings use the embedded spec; sidecars are exports,
not fallback metadata. Deploys, calls, and onchain inspection use public RPC.

The artifact library defines the Rust format implementation shared by the CLI
and standalone lowerer. Instruction and data memory use separate address spaces;
the lowerer preserves the data addresses chosen by the linker. Guest code
dispatches contract methods. Artifact changes must agree with the SDK's linker
and startup code and hogd's loader.

Builds run without chain state. Submission checks use the target chain's
configuration, and hogd decides admission and execution. The signing library
constructs transactions; the CLI selects keys, wallets, and profiles. The
embeddable client provides invocation without CLI configuration or process
supervision. Generated TypeScript bindings use `@thruputchain/client`.

The toolchain library installs the devnode bundle from a pinned catalog and
checks its downloaded contents. Runtime lookup is local; installation is
explicit. The supervisor runs the bundle as a child process.

## Choosing a test tool

| Tool | Use it to test |
| --- | --- |
| `thruput test` | A contract through the executor-backed project test runner |
| `thruput test --integration` | A project through a real local node |
| `thruput-testkit` | Several application actors and their interactions |
| `thruput-bench` | An RPC workload under controlled load |

Testkit owns actor coordination and can start a local node or attach to an
existing endpoint. Seeded accounts and randomness make inputs repeatable; they
do not make concurrent execution deterministic. Use messages and observed chain
results to order dependent actions. See its [Rustdoc](../crates/testkit/src/lib.rs)
for examples.

## Source map

| Area | Start here |
| --- | --- |
| Public methods and wire version | [json-rpc-api](../crates/json-rpc/json-rpc-api/src/lib.rs) |
| Payloads and codecs | [json-rpc-types](../crates/json-rpc/json-rpc-types/src/lib.rs) |
| Shared RPC server | [json-rpc-server](../crates/json-rpc/json-rpc-server/src/native/service.rs) |
| Backend response mapping | [hogd-backend](../crates/x23/hogd-backend/src/lib.rs) |
| Internal transport | [x23-transport](../crates/x23/x23-transport/src/lib.rs) |
| Standalone gateway | [gateway](../crates/json-rpc/json-rpc-gateway/README.md) |
| Local runtime lifecycle | [node-supervisor](../crates/node-supervisor/src/lib.rs) |
| CLI commands and configuration | [app.rs](../crates/cli/src/app.rs), [config](../crates/cli/src/config/mod.rs) |
| Terminal output and prompts | [print.rs](../crates/cli/src/print.rs) |
| Invocation and signing | [client](../crates/client/README.md), [signing](../crates/signing/src/lib.rs) |
| Runtime installation | [toolchain](../crates/toolchain/src/lib.rs) |
| Artifact format and lowering | [format.rs](../crates/rv32/artifact/src/format.rs), [lowerer](../crates/rv32/lower/src/lib.rs) |
| Application tests | [testkit](../crates/testkit/README.md) |
| Committed-block streaming | [firehose](../crates/firehose/README.md) |
| Hosted funding service | [faucet](../crates/faucet/README.md) |

[Cargo.toml](../Cargo.toml) lists all crates. The [crate layout](../crates/README.md)
explains their directory groups.
