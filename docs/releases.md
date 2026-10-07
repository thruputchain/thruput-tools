# Releases

This procedure is for maintainers with access to the release workflows and
publishing credentials. Building or contributing to the repository does not
require publishing a release.

Run the **Release package** GitHub Actions workflow for a normal release.

- [`publish.yml`](../.github/workflows/publish.yml) publishes the Rust crates to the private
  `groundhog` Cargo registry, creates and pushes the matching `vX.Y.Z` tag, then
  dispatches binary packaging.
- [`release.yml`](../.github/workflows/release.yml) is dispatch-only and internal to
  `publish.yml`. Do not run it directly for normal releases.

Before release, bump the workspace version in `Cargo.toml` and keep internal
workspace dependency pins aligned. Do not manually push `v*` tags.

Release assets:

```text
<linux-app>-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
<linux-app>-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz
groundhog-vX.Y.Z-aarch64-apple-darwin.tar.gz
SHA256SUMS
```

Each binary archive also has a `<binary-archive-base>-debuginfo.tar.gz`
companion covered by `SHA256SUMS` (24 archives total).

- The trailing `-debuginfo` prevents it matching binary selectors ending in the
  target or `-znver5`.
- Linux binaries retain `.symtab` and carry a GNU debuglink. Their split debug
  files are rooted at `usr/lib/debug/.build-id/<xx>/<rest>.debug`, using the
  binary's GNU build-id.
- The macOS companion contains a `groundhog.dSYM` bundle. Its binary is stripped
  of debug information and re-signed ad hoc.
- Release builds include line tables and force frame pointers.

Install the Linux debuginfo companion with
`sudo tar -C / -xzf <archive>-debuginfo.tar.gz` to place its files under
`/usr/lib/debug/.build-id/<xx>/<rest>.debug`, the default build-id debug root
for gdb, perf, and eBPF profilers. Alternatively, extract it elsewhere and point
the tool's debug-file directory at the extracted `usr/lib/debug` tree.

Linux apps are `hog-gateway`, `groundhog`, `rv32-lower`, `hogsim`, and
`groundhog-faucet`. Each archive contains one app binary plus `README.md` and
`LICENSE`. `hog-gateway`, `hogsim`, and `groundhog-faucet` are binary release
artifacts only; `groundhog-rv32-lower` is also a published crate/library. macOS
releases currently target Apple Silicon and include only the `groundhog` CLI,
which links the RV32 lowerer in-process. The macOS binary is ad-hoc signed but
not notarized, so macOS may require explicit approval after download.

## AMD server gateway artifact

Linux releases additionally publish
`hog-gateway-v<VERSION>-x86_64-unknown-linux-gnu-znver5.tar.gz`, covered by the
release `SHA256SUMS`. This gateway is compiled with `target-cpu=znver5` for
qualified AMD Zen 5 servers (for example EC2 C8a/M8a). Packaging must select it
explicitly and validate it on a Zen 5 host before deployment. Portable gateway,
CLI, faucet, simulator, and macOS artifacts keep their existing CPU targets.
