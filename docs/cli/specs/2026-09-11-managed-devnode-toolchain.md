# Managed devnode toolchain: implementation and qualification

GH-232 continues on `mm/gh-232-toolchain`, based on `9ff3c9b`.
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
cleanup share a cancellable per-version/target lock. A staged bundle is renamed
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

## Critical review changes

- Fixed stale saved selection blocking `use <new-version>` recovery.
- Made explicit version selection override environment/sibling discovery.
- Added managed-cache discovery to status without mutation.
- Made acquisition and lock waits cancellable during node startup.
- Narrowed offline wording to the implemented network boundary.
- Allowed lock-aware removal of retired versions and fixed which output.
- Required the current genesis artifact alongside the wallet artifact.
- Added publication gates using the actual installer for all targets, avoiding
  a second archive parser, and real startup/RPC/offline-restart qualification.

## Trusted catalog provenance

The candidate catalog uses public hog-devnode `0.1.27-rc.4`.
Its manifest was retrieved from authenticated packaging workflow artifact
`public-hog-devnode-v0.1.27-rc.4`, run `34167428166`, artifact `10034650254`.
The run succeeded at packaging commit
`b73a88da945469ed3c72b03e5f70176dc48bb686`.
The authenticated manifest and public HTTPS manifest have identical SHA-256:
`0c95c0a6d3e4256a68b297492b7bdb1b6217fcf1bedcaf98df5ea8db7029bc07`.

The catalog copies each target's archive, objectKey, size, sha256, layoutVersion,
files, and compatibility fields, plus source repository/ref, into catalog
schema 1. Packaging manifest schema 3 includes additional publication evidence;
it is not the same schema as the embedded catalog. The signed macOS derivative
has different bytes from the original hogd archive, so its packaging output is
the trust handoff. All three public target archives passed the real installer.

## Unresolved release blocker

**This candidate is not ready to merge or publish.** The catalog version is a
verified acquisition candidate, not a qualified default for current main.
Fresh-cache startup without overrides fails with:

```
hog-devnode runtime manifest is missing endpoints.schedulerGateway
```

The bundle's hogd tag pins protocol `236f9821a6244aff27ec0fa014a98ea4ad4c0326`;
current groundhog-xdr 0.35.0 advertises `hogp/63c9284d8d5727e`.
Do not suppress manifest/protocol validation or downgrade current dependencies
to make this bundle start. Publish a matching hogd bundle through the existing
packaging flow, import its authenticated catalog fragment, and run the gates.

A concrete producer source candidate is hogd `b10b8cc2bd4bf4bacb3ff97ec13673b92ace2559`
(live main verified during this work). Its committed protocol gitlink is exactly
`63c9284d8d5727e8355b0b3d01e5e40f15478a89`, and
`hogd/devnode/runtime_manifest.cc` emits `schedulerGateway`. This establishes
source alignment only; it still needs normal build/release qualification.
Use an isolated checkout with exact submodules: the existing local hogd working
checkout has submodule changes and must not be used as evidence for that commit.

The pre-publication workflow now runs, before creating a tag or publishing:

1. Rust catalog contract tests.
2. Ignored `public_catalog_installs_all_targets` using production Installer and
   the fixed public origin for the default across Linux x86_64/ARM64 and macOS ARM64.
3. `scripts/smoke-managed-toolchain.sh`: fresh-cache startup, chainTip RPC,
   clean stop, offline persistent restart, and non-mutating managed status.

The smoke clears binary/protocol/mirror overrides, copies the CLI away from
sibling binaries, isolates XDG directories, and cleans all processes in its
owned session on failure. Startup qualification must pass before release.
Linux runtime qualification still needs supported Linux hosts; downloading all
three targets on macOS verifies bytes/layout, not Linux execution.

## Validation commands

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p groundhog-toolchain --test install_contract public_catalog_installs_all_targets -- --ignored
scripts/smoke-managed-toolchain.sh
cargo run -p groundhog-agent-knowledge -- check
```

When the registry migration blocked online dependency fetches, tests and clippy
used command-only source replacement pointing to the existing groundhog.dev
cache with Cargo `--offline`. Repository registry configuration and locked
package versions/checksums were preserved. A stale inherited token was then
identified: the environment token returned 403, while the saved registry token
returned 200. No credentials were written or rotated by this work.

## Current verification result

- Workspace tests, workspace Clippy with all targets and warnings denied,
  formatting, and knowledge-bundle validation passed.
- All three public bundles passed the real installer in an empty cache.
- Fresh-cache macOS startup failed at the runtime-manifest boundary shown above;
  offline restart and real RPC qualification remain unverified.
- Removing the stale environment token for the command only allowed a complete
  locked fetch and workspace build against crates.thruput.network to pass.
- Independent slice reviews and a fresh whole-change review completed. Findings
  were addressed, including smoke descendant cleanup and deriving the tested
  executable from Cargo build output instead of assuming a target directory.
