# Managed devnode toolchain: implementation and qualification

GH-232 continues on `mm/gh-232-toolchain`, rebased onto `6d38333` (RPC 0.17.0).
The August implementation is preserved on
`gh-232-managed-devnode-toolchain-port` at `572be4f`.

## Contract

Manage one acquisition unit: hog-devnode, dev_wallet.rv32a,
genesis_root.rv32a, and the catalog-declared macOS runtime libraries.
The embedded catalog authorizes exact versions/targets/bytes. It is committed
before tagging. A different mirror never authorizes different bytes.

A command-line managed version takes precedence over implicit local binaries.
Otherwise explicit binary, environment, and sibling paths remain escape hatches,
followed by selected managed cache/acquisition and finally PATH. A saved managed
selection cannot silently fall back to PATH. The explicit binary and version
flags conflict. Status discovers the same local sources without acquisition.

Offline applies to managed acquisition, not to RPC or Cargo traffic. Downloads
verify size and SHA-256 before bounded allowlisted extraction. Installation and
cleanup share a per-version/target lock. Installation and lease waits are
cancellable; cleanup fails promptly while a node or installer holds the lock.
Managed startup transfers its shared lease to the supervised child before
readiness waits. Cancelled startup or shutdown and plain handle drop retain the
lease in a synchronous reaper thread until the child and its process group exit,
independently of the caller's Tokio runtime. Concurrent nodes share leases. A staged bundle is renamed
only after completion metadata is durable. The local cache is trusted;
reuse checks completion metadata and file presence, not installed-file hashes.

The source belongs to acquisition, not to cache lookup. `Installer` has no
source lifetime or generic parameter; only `install` receives a source.
This keeps list/which/clean/status independent of HTTP client configuration.

The catalog is an allowlist, not a remotely extensible release index. An obsolete
saved selection is visible in list, recoverable through use, and removable
through clean even after the version leaves the catalog. Cleanup validates the
version as a safe path component and cannot turn it into an arbitrary path.
`which` and JSON `path` identify the executable, not the bundle directory.

Managed selection checks the actual host glibc/macOS version against the catalog
minimum before accepting either cached or downloaded bytes. Probe failure is
an incompatibility error, not permission to proceed. Only implicit default
selection may fall back to PATH; explicit or saved selections fail clearly.
Explicit binary, environment, and sibling paths continue bypassing acquisition.
`list` and `clean` remain available regardless of runtime compatibility.

## Critical review changes

- Fixed stale saved selection blocking `use <new-version>` recovery.
- Made explicit version selection override environment/sibling discovery.
- Added managed-cache discovery to status without mutation.
- Made acquisition and lock waits cancellable during node startup.
- Narrowed offline wording to the implemented network boundary.
- Allowed lock-aware removal of retired versions and fixed which output.
- Required the current genesis artifact alongside the wallet artifact.
- Enforced catalog runtime minimums before managed selection.
- Protected bundles with shared node-lifetime leases and refused busy cleanup.
- Kept concurrent installers able to reuse bundles already leased by nodes.
- Added publication gates using the actual installer for all targets, avoiding
  a second archive parser, and real startup/RPC/offline-restart qualification.

## Trusted catalog provenance

Platform bundle 0.1.27 pins public hog-devnode `0.1.27-rc.7`; the CLI uses
that exact component version, not the platform version.
Its manifest was retrieved from authenticated packaging workflow artifact
`public-hog-devnode-v0.1.27-rc.7`, run `34737642408`, artifact `10311661280`.
The run succeeded at packaging commit
`8680a36d313c704d7cfaf5f8f408c129e3b43977`.
The authenticated manifest and public HTTPS manifest have identical SHA-256:
`5e71d0df7e65e0858d1e5e92ad0a19a1c8d6d3aa65b609f00ae6a0b87cab0673`.

The catalog copies each target's archive, objectKey, size, sha256, layoutVersion,
files, and compatibility fields, plus source repository/ref, into catalog
schema 1. Packaging manifest schema 3 includes additional publication evidence;
it is not the same schema as the embedded catalog. The signed macOS derivative
has different bytes from the original hogd archive, so its packaging output is
the trust handoff. All three public target archives passed the real installer.

## Release qualification

The rc.7 bundle resolves the earlier missing `schedulerGateway` manifest
endpoint. Its protocol matches current main. The real managed startup smoke
now passes on macOS, native Linux ARM64, and emulated Linux x86_64 without
a binary or protocol override, including RPC,
clean shutdown, offline persistent restart, and local status discovery.

The pre-publication workflow now runs, before creating a tag or publishing:

1. Rust catalog contract tests.
2. Ignored `public_catalog_installs_all_targets` using production Installer and
   the fixed public origin for the default across Linux x86_64/ARM64 and macOS ARM64.
3. `scripts/smoke-managed-toolchain.sh`: fresh-cache startup, chainTip RPC,
   clean stop, offline persistent restart, and non-mutating managed status.

The smoke clears binary/protocol/mirror overrides, copies the CLI away from
sibling binaries, isolates XDG directories, and cleans all processes in its
owned session on failure. Startup qualification must pass before release.
Linux qualification used Ubuntu 26.04 for native ARM64 and emulated x86_64.
The x86_64 result verifies execution under emulation, not native host behavior.

## Validation commands

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p groundhog-toolchain --test install_contract public_catalog_installs_all_targets -- --ignored
scripts/smoke-managed-toolchain.sh
cargo run -p groundhog-agent-knowledge -- check
```

## Current verification result

- The catalog pin test first failed against rc.4, then passed with the exact
  authenticated rc.7 manifest records. Independent catalog review found no
  mismatched target metadata or trust-boundary changes.
- All three public bundles passed the real installer in an empty cache.
- Fresh-cache macOS startup, chainTip RPC, clean shutdown, offline recovery,
  and non-mutating managed status passed without overrides.
- Native Linux ARM64 and emulated x86_64 managed startup and offline recovery
  passed on Ubuntu 26.04, along with all ten CLI toolchain integration tests
  on each target.
- Workspace tests, warning-free build, all-target Clippy, formatting, and agent
  knowledge checks passed. Final independent whole-change review found no
  actionable issues.
- The macOS local-DX smoke passed using the installed managed binary: scaffold,
  build, deploy/call/view/storage, transfers/history/events, saved-wallet
  creation and funding, and dump/restore with state recovery and further calls.
  Cargo used an isolated configuration with one registry alias to avoid a
  duplicate alias in the developer configuration; no protocol override was used.

The reviewer fixes additionally passed runtime-version/probe-failure fixtures,
shared-lease cleanup tests, and the concurrent installer reuse regression.
Updated managed smokes on macOS and both Linux targets verified busy cleanup
while running and successful cleanup after shutdown. Older-host rejection is
covered by probe fixtures, not execution on a real below-minimum host.

Drop/cancellation qualification passed on macOS, native Linux ARM64, and emulated
Linux x86_64: delayed plain drop, destroyed caller runtime, cancelled shutdown,
and surviving descendants retain the process guard. A CLI test with a real
managed lease also proves cancelled startup remains busy after runtime teardown,
then permits cleanup after the child exits. The managed runtime smoke and full
workspace checks passed after these changes.
