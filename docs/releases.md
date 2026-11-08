# Publish a release

This procedure is for maintainers with release workflow and publishing access.
See the [changelog](../CHANGELOG.md) for upgrade notes.

## Prepare and publish

1. Bump the workspace version in [Cargo.toml](../Cargo.toml) and align internal
   dependency pins. SDK and protocol pins must already exist on crates.io.
   Check the generated project's SDK pin in
   [scaffold.rs](../crates/cli/src/scaffold.rs) and refresh `Cargo.lock` after
   dependency changes.
2. Merge the version change through a PR and let CI pass.
3. Dispatch **Release package** ([publish.yml](../.github/workflows/publish.yml))
   from `main` with `dry-run` enabled. This runs checks without tagging or
   publishing.
4. Dispatch it again from `main` without `dry-run` to publish. The workflow
   creates the version tag, publishes crates, dispatches binary packaging, and
   tests the published CLI with generated projects.

To resume a partial release, dispatch the same workflow from `main` with
`release-ref` set to the existing `vX.Y.Z` tag. The selected source must belong to
`main` history. Do not push release tags manually or change existing tags.

[release.yml](../.github/workflows/release.yml) packages binaries and creates the
GitHub Release. Let `publish.yml` dispatch it. Its workflow definition runs from
`main` and checks out the selected tag.
[yank.yml](../.github/workflows/yank.yml) yanks or restores a version across the
published crates.

These guards apply to the current workflows. Historical tags keep their original
workflow definitions; restricting their execution requires separate repository
controls.

## GH-554 format transition

The agreed sequence is runtime-first. Qualify the combined candidate tools and
hogd before publishing. v6 uses a 128-byte header followed by code, initial memory,
and an XDR extension list. Existing v5 contracts remain readable and executable.

1. Build the dual-format hogd runtime with published tools 0.43.0. Publish the
   compatible runtime, generator, and test executor after review and green CI.
2. Release the Rust SDK with that executor.
3. Pin the runtime and verified asset digests in the tools catalog and update
   the scaffold SDK pin. Require normal green CI before merging and publishing
   tools 0.44.0. Managed-runtime startup remains a required publication check.
4. Align the TypeScript packages and qualify the exact published stack. Upgrade
   and verify the network before deploying v6 contracts or shipping explorer.

New optional metadata kinds need no format change. Execution changes still need
an explicit compatibility and activation plan. Preserve existing network state.
Remove v5 support separately, after an approved reset or verified migration.

## Publishing credentials

Existing crates use crates.io trusted publishing through GitHub OIDC. Each
crate's trusted publisher must name this repository, `publish.yml`, and the
`release` environment.

Keep these tokens in the `release` environment when needed:

- `CRATESIO_CREATE_PACKAGE_TOKEN`: creates new crates, registers trusted
  publishers, and adds `CRATE_OWNERS`. It needs `publish-new`, `trusted-publishing`,
  and `change-owners` scopes. Remove and revoke it once all crates exist.
- `CRATESIO_YANK_TOKEN`: used by `yank.yml`, with the `yank` scope. Trusted
  publishing tokens cannot yank.

## Binary artifacts and symbols

The [release workflow matrix](../.github/workflows/release.yml) defines the
binaries and target platforms. Each binary archive includes a README and license.
`SHA256SUMS` covers the binaries and their `-debuginfo.tar.gz` companions.

Linux companions contain split debug files under
`usr/lib/debug/.build-id/`. Install one with:

```sh
sudo tar -C / -xzf /path/to/binary-debuginfo.tar.gz
```

Alternatively, extract it elsewhere and point the profiler or debugger at that
`usr/lib/debug` tree. Match symbols to the binary's GNU build ID. The macOS
companion contains a `thruput.dSYM` bundle; its binary is signed ad hoc and is not
notarized.

Gateway archives with a `-znver5` suffix require AMD Zen 5 hosts. Select them
explicitly and validate them on that CPU before deployment.
