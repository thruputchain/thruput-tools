# groundhog-rv32-lower

Turn a freestanding `riscv32im-unknown-none-elf` contract binary into a
Groundhog **v3 RV32 artifact** (`.rv32a`).

This is the production successor to hogd's `scripts/rv32_lower.py` — the
`wasm-opt` analog for the rv32 build path. It is a focused, standalone tool that
the build flow shells out to (rather than embedding), so the artifact format has
exactly one Rust implementation.

## Usage

Build the contract's rv32 bin crate (the SDK per-contract `<contract>-rv32`
wrapper — e.g. sdk_rs `example-contracts/counter-rv32`), then lower it:

```sh
cargo build --release --target riscv32im-unknown-none-elf -p <contract>-rv32
rv32-lower --elf target/riscv32im-unknown-none-elf/release/<contract>-rv32 \
           --out <contract>.rv32a
```

(Invoking this from `groundhog build` as a `wasm-opt`-style shell-out is a later
phase — see the status note below.)

Optional build-metadata sidecar:

```sh
rv32-lower --elf <elf> --out <contract>.rv32a --metadata <contract>.json \
           --rustc-version "$(rustc --version)" \
           --target-triple riscv32im-unknown-none-elf \
           --build-flags release
```

For scripting / programmatic callers, `--json` emits a single-line result
envelope on stdout (output path, byte sizes, code/artifact digests, dispatch
table) on success, or `{"status":"error","message":…}` on failure:

```sh
rv32-lower --elf <elf> --out <contract>.rv32a --json
```

The lowerer:

1. Parses the ELF32 section and symbol tables.
2. Validates the layout (single `.text` at vaddr 0, allowlisted alloc sections,
   no relocations, no data in `.text`, …) and materializes the 64 KiB
   initial-memory image.
3. Discovers `#[contract]` dispatch entries — the SDK macro emits each method as
   an `#[export_name = "<16-UPPER-HEX>"]` symbol — and maps `method_id → pc`.
4. Packs the v3 header + sections, hashes each section, and self-verifies.

## Library

The crate is also a library (`groundhog_rv32_lower`):

- `lower(&[u8]) -> Result<Lowered>` — the whole pipeline.
- `artifact` — the v3 format: constants, writer, and `verify_artifact`. **This
  module is the single Rust source of truth for the on-disk format** and mirrors
  hogd's `hogd/hogexec/runtime/rv32/artifact.h`.
- `elf` — a minimal, dependency-free ELF32-LE-RISC-V reader.
- `metadata` — the JSON sidecar (byte-compatible with the python script).

## Tests

```sh
cargo test -p groundhog-rv32-lower
```

The suite is hermetic: it builds minimal synthetic ELFs in-process (no riscv
toolchain needed) and exercises the happy path plus every validation/rejection
rule. To also byte-compare against a real contract / a `rv32_lower.py` golden:

```sh
RV32_LOWER_PARITY_ELF=/path/to/<contract>-rv32 \
RV32_LOWER_PARITY_GOLDEN=/path/to/golden.rv32a \
  cargo test -p groundhog-rv32-lower
```

(unset → that test skips, the same opt-in / out-of-default-CI posture as the
hogd Spike oracle).

## Status and future home

See `hogd/docs/rv32/rv32-author-toolchain.md` for the full design.

It is **published to the private `groundhog` registry** and consumable two ways:

- **as a binary** — `cargo install --registry groundhog groundhog-rv32-lower`
  puts `rv32-lower` on `$PATH`; hogd's smoke test / `groundhog build` shell out
  to it the way they do `wasm-opt`;
- **as a library** — add `groundhog-rv32-lower` (registry `groundhog`) as a
  dependency and call `lower()` / the `artifact` + `metadata` modules directly
  (e.g. from sdk_rs).

The crate is self-contained, so it can still move to its own repo later; the
`artifact` module is slated to split out to a standalone `groundhog-rv32-artifact`
crate (next to `groundhog-xdr` in protocol-rs) with a build-time digest
cross-check against hogd's `artifact.h`.
