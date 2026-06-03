# Lowerer: accept the Harvard-split data layout (kill the `.text` shadow)

Status: accepted, 2026-06-02. Scope: `groundhog-rpc` lowerer only. The
companion `sdk_rs` linker change is tracked separately (in flight).

## Problem

The Groundhog RV32 VM is modified-Harvard: code is fetched by PC from the
artifact's `code` section, loads/stores hit the separate `initial_memory`
section. The two are independent address spaces in the VM.

Today `sdk_rs`'s `groundhog-rv32.ld` lays everything into one 0-based region
with `.text` first, so data sections get vaddrs *above* `code_size`. The stored
data image is address-indexed from 0, so it carries a code-sized leading run of
zeros — the inert "shadow" of `.text` underneath the real data. On the counter
smoke contract the data image is ~12.7 KB of which ~12.5 KB is that shadow; the
code is effectively shipped twice.

The fix (in `sdk_rs`) is to give data its own low-VMA region with a small guard
at the bottom, exploiting the Harvard split: data vaddrs can numerically overlap
the code's PC range because they live in different VM memories. With data based
low, the address-indexed image is compact from the start.

## Why this is a lowerer change at all

The lowerer is a pure `ELF → artifact` function: it copies each data section to
`memory[section.addr ..]` at the vaddr the linker assigned. It cannot relocate
data (the addresses are baked into the compiled load/store instructions). So the
byte savings come entirely from the `sdk_rs` linker placing data low.

But the lowerer currently *rejects* the layout the linker change would produce:

```rust
if section.addr < text_end {
    return Err(invalid(".. data section overlaps .text range .."));
}
```

With data based at vaddr 8 and `.text` spanning `[0, code_size)`, every data
section trips this. The lowerer must be taught that numeric overlap with the PC
range is legal under the Harvard split — otherwise the linker change produces
ELFs the lowerer refuses.

## Decision

Relax the lowerer to accept the Harvard-split layout, while reserving the bottom
of data memory for a null guard. No artifact-format change.

1. **Drop the `addr < text_end` rejection.** Code and data are separate VM
   address spaces; a data vaddr inside the code's numeric PC range is legal. The
   `text_end` local and the text-overlap branch are removed.

2. **Add a null-guard rule.** Reject any data section whose address range
   touches `[0, 8)` (i.e. `section.addr < 8`). hogd stages `initial_memory` at
   data address 0 and zero-extends; a static landing at address 0 would make a
   Rust `&'static` to it a readable null pointer (UB). The `sdk_rs` data region
   starts at an 8-byte guard, so real contracts clear this; the lowerer enforces
   the invariant at the point the artifact is produced. The guard size (8) is
   fixed to match the planned linker `ORIGIN`.

3. **Everything else is unchanged:** the PROGBITS allowlist, data-vs-data
   overlap detection, the `[0, high_water)` stored-prefix logic, NOBITS handling,
   and `memory_bytes = MAX_MEMORY_BYTES`. `FORMAT_VERSION` stays 4 and the
   format fingerprint is untouched.

## Consequence (honest)

After this change alone, real contracts produce **byte-identical** artifacts —
the current linker still stacks data above `.text`, so the lowerer sees the same
high vaddrs and emits the same shadowed image. The shrink materializes only once
the `sdk_rs` linker change lands and contracts are rebuilt. This PR is the
necessary unblock plus the regression proof that the lowerer handles the new
layout; the counter-contract byte-savings verification runs in the `sdk_rs` PR.

## Verification (in this repo)

Hermetic synthetic-ELF tests in `crates/rv32/lower/tests/lower.rs`
(no riscv toolchain needed):

- **New:** `.rodata` at vaddr `0x8` while `.text` spans `[0, 0x20)`. Assert the
  stored prefix is ~12 bytes (`0x8 + 4`), not code-sized, and that the bytes
  land at offset `0x8`. Proves numeric overlap with the PC range is accepted and
  the image stays compact.
- **Updated:** `rejects_data_section_overlapping_text` becomes a null-guard
  test — vaddr-0 data is rejected with the null-guard message, not the old
  "overlaps .text" message. Add an `addr = 4` case (inside the guard, above 0)
  to pin `addr < 8`.
- Unchanged: data-vs-data overlap, allowlist, NOBITS, memory-limit,
  format-fingerprint, and `--json` tests continue to pass.

## Out of scope

- The `sdk_rs` `groundhog-rv32.ld` low-VMA + 8-byte-guard change (in flight).
- Any artifact-format / hogd-runtime change. hogd already stages
  `initial_memory` at data address 0 and zero-extends; it receives a smaller
  image, nothing else.
