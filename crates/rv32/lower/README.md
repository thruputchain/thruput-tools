# thruput-rv32-lower

Turn a freestanding `riscv32im-unknown-none-elf` contract binary into a
Thruput **v5 RV32 artifact** (`.rv32a`).

This focused lowerer validates the contract ELF and emits the canonical RV32
artifact format. It has two entrypoints: a standalone `rv32-lower` binary for
scripts and interop checks, and a library API used by `thruput build`. The
artifact format has exactly one Rust implementation.

## Usage

Build the contract's rv32 bin crate (the SDK per-contract `<contract>-rv32`
wrapper — e.g. sdk_rs `example-contracts/counter-rv32`), then lower it:

```sh
cargo build --release --target riscv32im-unknown-none-elf -p <contract>-rv32
rv32-lower --elf target/riscv32im-unknown-none-elf/release/<contract>-rv32 \
           --out <contract>.rv32a
```

`thruput build` invokes the same lowerer in-process through the
library API; use the binary directly when you need a scriptable lower-level
tool.

Optional build-metadata sidecar:

```sh
rv32-lower --elf <elf> --out <contract>.rv32a --metadata <contract>.json \
           --rustc-version "$(rustc --version)" \
           --target-triple riscv32im-unknown-none-elf \
           --build-flags release
```

The sidecar includes `artifact_format_fingerprint`, the pinned v5 format
contract digest that hogd cross-checks against its C++ runtime constants. To
print that value without lowering an ELF:

```sh
rv32-lower --print-format-fingerprint
```

For scripting / programmatic callers, `--json` emits a single-line result
envelope on stdout (output path, byte sizes, code/artifact digests) on success,
or `{"status":"error","message":…}` on failure:

```sh
rv32-lower --elf <elf> --out <contract>.rv32a --json
```

The lowerer:

1. Parses the ELF32 section table.
2. Validates the layout (entry point 0, single `.text` at vaddr 0, allowlisted
   alloc sections, no relocations, …) and materializes the code plus the
   initial-memory high-water prefix. v4 dispatch lives in the guest, so no
   symbol scanning is needed and stripped release builds are valid inputs.
3. Packs the v5 header + sections, hashes each section, and self-verifies.

## Library

The crate is also a library (`thruput_rv32_lower`):

- `lower(&[u8]) -> Result<Lowered>` — the whole pipeline.
- `thruput-rv32-artifact` — the v5 format crate: constants, writer, and
  `verify_artifact`. **This crate is the single Rust source of truth for the
  on-disk format** and mirrors hogd's `hogd/hogexec/runtime/rv32/artifact.h`.
- `elf` — a minimal, dependency-free ELF32-LE-RISC-V reader.
- `metadata` — the JSON build-info sidecar emitted next to the artifact.

## Tests

```sh
cargo test -p thruput-rv32-lower
```

The suite is hermetic: it builds minimal synthetic ELFs in-process (no riscv
toolchain needed) and exercises the happy path plus every validation/rejection
rule. To also byte-compare against a real contract / a known-good golden artifact:

```sh
RV32_LOWER_PARITY_ELF=/path/to/<contract>-rv32 \
RV32_LOWER_PARITY_GOLDEN=/path/to/golden.rv32a \
  cargo test -p thruput-rv32-lower
```

(unset → that test skips, the same opt-in / out-of-default-CI posture as the
hogd Spike oracle).

## Status and future home

See `hogd/docs/rv32/rv32-author-toolchain.md` for the full design.

It is **published to crates.io** and consumable two ways:

- **as a binary** — `cargo install thruput-rv32-lower`
  puts `rv32-lower` on `$PATH`; hogd's smoke tests and other scripts can call it
  directly;
- **as a library** — add `thruput-rv32-lower` as a
  dependency and call `lower()` / `metadata` directly. `thruput build` uses
  this path. Add
  `thruput-rv32-artifact` for direct access to format constants, writer, and
  validator APIs.

The format surface lives in `thruput-rv32-artifact` under `crates/rv32/artifact`;
`rv32-lower` depends on it instead of defining or re-exporting the format.
