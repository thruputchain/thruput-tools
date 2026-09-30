# CLI design

The CLI joins project discovery, configuration, builds, signing, and public RPC
into a contract-development workflow. Command syntax belongs in the
[CLI reference](../../crates/tools/cli/README.md) and installed `--help` output.
This guide explains the boundaries to preserve when changing that workflow.

## Configuration describes intent; runtime state describes a running node

Global configuration holds reusable profiles and user defaults. Project
configuration holds project-specific choices, contract aliases, and overrides.
Explicit command options override saved choices. Config inspection preserves
where values came from so a user can distinguish a saved value from an effective
one without exposing credentials.

A running local node is discovered through global `local_runtime_state`.
Starting a node does not rewrite the user's selected profile into a local one.
Profiles with local network semantics can consume that runtime state. The node
clears only its matching state on shutdown, so an older process cannot erase a
newer node's discovery record.

Read [config](../../crates/tools/cli/src/config/mod.rs) for resolution and
persistence, and [node.rs](../../crates/tools/cli/src/node.rs) for runtime-state
ownership. Keep inspection read-only; recovery or configuration changes require
an explicit action.

## Wallet identity survives changes in defaults

A signing key and a wallet binding are different things. Native accounts can
be derived and saved without creating an on-chain object or funding it. Funding
is a separate explicit operation. Existing legacy bindings and manifest-backed
`dev-N` wallets preserve their identities and transaction formats; changing the
default wallet implementation must not silently remap them.

Native deployment transfers an explicit amount and can initialize the contract
in the same transaction. Atomic funding avoids leaving a prefunded address
stranded when deployment fails. The amount is an input, not an estimate of future
storage cost. An ambiguous submission needs reconciliation by its canonical
transaction hash before another attempt.

The [signing library](../../crates/tools/signing/src/lib.rs) owns transaction
construction. CLI wallet selection, funding, and deployment must use the same
resolved identity. These client semantics do not by themselves migrate a hosted
chain's bootstrap artifacts or retained state; those are separate runtime and
operations decisions.

## Builds and generated interfaces

Cargo owns compiler output. The CLI owns derived contract artifacts, leaving
Cargo's raw outputs intact. Shared artifact resolution keeps build, test, deploy,
and call consistent across packages, profiles, and target directories.
The [artifact module](../../crates/tools/cli/src/artifacts.rs) owns those paths.

The RV32 lowerer is both a library used by the CLI and a standalone tool. A
contract's `.spec.xdr` interface drives codecs and generated bindings; the CLI
must not invent a parallel ABI. Generated TypeScript describes a contract's
methods and events and delegates invocation to `@groundhogchain/client`.
See [codegen.rs](../../crates/tools/cli/src/codegen.rs), its consumer fixtures,
and [RV32 design](../rv32/design.md).

Builds remain independent of chain admission. The target chain's current
configuration determines execution bounds at submission; offline compilation
cannot promise that every network will accept the artifact.

## Runtime acquisition has an explicit trust boundary

The [toolchain crate](../../crates/tools/toolchain/src/lib.rs) manages a complete
devnode bundle, including its required contract artifacts and runtime libraries.
Its embedded catalog authorizes exact versions, targets, sizes, and digests.
Changing the download mirror does not authorize different bytes.

Discovery and status stay local and non-mutating; acquisition is a separate
operation. Installation verifies downloaded content before publishing a complete
cache entry. Reuse trusts the local cache's completed installation, rather than
rehashing every file on startup. Version selection, supported hosts, and override
precedence belong in the [toolchain reference](../../crates/tools/cli/README.md#groundhog-toolchain).

A managed node holds a shared usage lease inherited by the child process.
Cleanup requires the exclusive lock. This protects bundle files even if the CLI
parent exits while the child remains alive. The lock must stay held until all
inheriting descriptors close; explicitly unlocking the shared open-file
description in the parent would defeat that protection. See
[install.rs](../../crates/tools/toolchain/src/install.rs) and its lifecycle tests.

## Human interaction and automation share command logic

`Print` owns prompting and rendering policy. Non-interactive operation disables
prompts without granting consent. Structured output uses typed command results
so scripts can distinguish failure from human diagnostics. Child processes must
honor the same terminal policy. Keep this in
[print.rs](../../crates/tools/cli/src/print.rs) rather than implementing terminal
checks separately in each command.
