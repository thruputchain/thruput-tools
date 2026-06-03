# RV32 v4 Artifact Cut — lowering + artifact components

Status: design (2026-06-02). Execution contract for the `groundhog-rpc`-side of
the "RV32 Guest-Side Dispatch and the v4 Artifact Cut" proposal. The SDK already
emits ELFs with guest-side `_start` dispatch; the hogd C++ consume-side and the
`Machine::seed_method_id` spike are out of scope here.

**Decision 2 is resolved: Option A (metered guest miss).** The dispatch section
is fully removed from the artifact, there is no host-side method-existence check,
and an unknown method id is handled by the guest dispatcher's default arm. This
fixes the v4 header layout (`header_bytes` 172 → 132).

## Scope

In scope (this repo):

- `crates/rv32/artifact` — the v4 format: drop the dispatch section, bump
  `format_version` 3 → 4 and `entry_abi_version` 1 → 2, relax the
  initial-memory rule (Decision 5), re-pin `FORMAT_FINGERPRINT`.
- `crates/rv32/lower` — thin the lowerer: validate the entry from the ELF header
  (`e_entry == 0`) and `.text` layout only; stop scanning `.symtab`; stop
  building a dispatch table; store the initial-memory high-water prefix instead
  of a padded 64 KiB image; drop `dispatch_entries` from metadata.
- `crates/tools/cli` — drop `CARGO_PROFILE_RELEASE_STRIP=false` from the rv32
  build path and refresh the lower-failure hint. `--features entrypoints` is
  left alone (SDK-owned).

Out of scope: hogd `runtime_context.{h,cc}` / `artifact.{h,cc}`,
`Machine::seed_method_id`, the SDK `_start` dispatcher, and the register-seed
feasibility spike. Those land in their own repos as part of the atomic cut.

## Decision summary (from the proposal)

- **Decision 1 — entry ABI 1 → 2.** Runtime enters at PC 0 and seeds the method
  id in `a0`/`a1`. No artifact change beyond the `entry_abi_version` stamp; the
  lowerer already requires `e_entry == 0` and `.text` at vaddr 0.
- **Decision 2 — Option A (metered guest miss).** No dispatch table, no host
  existence check. Unknown methods are a guest concern.
- **Decision 3 — format v4.** Dispatch section removed; `header_bytes` 132.
- **Decision 5 — zero-extended initial memory.** Store only `[0, high_water)`;
  the runtime zero-fills `[initial_memory_size, memory_bytes)`.
- **memory_bytes.** Stays the full 64 KiB address space (`= MAX_MEMORY_BYTES`,
  the `sp` top). The v4 verifier is per-contract *capable* (accepts any nonzero
  `memory_bytes ≤ MAX`), but the lowerer continues to stamp `MAX_MEMORY_BYTES`
  because there is no per-contract RAM-length source in the ELF.

## v4 header layout (132 bytes, little-endian, no padding)

```text
  off  size  field                      vs v3
    0     8  magic "GHRV32A1"           unchanged
    8     4  format_version      (4)    value 3 → 4
   12     4  header_bytes        (132)  value 172 → 132
   16     8  vm_profile_digest          unchanged
   24     4  flags               (0)    unchanged
   28     4  entry_abi_version   (2)    value 1 → 2
   32     4  syscall_abi_version (1)    unchanged
   36     4  gas_table_version   (2)    unchanged
   40     8  gas_schedule_digest        unchanged
   48     4  memory_bytes               value unchanged (64 KiB); verifier accepts nonzero ≤ MAX
   52     4  code_size                  unchanged
   56     4  initial_memory_size        now ≤ memory_bytes (was ==)
   60     4  code_offset         (132)  value 172 → 132
   64     4  initial_memory_offset      value shifts (no dispatch gap)
   68    32  sha256(code)               position was 76
  100    32  sha256(initial_memory)     position was 108
```

Body: `header || code || initial_memory`. Removed from v3: `dispatch_offset`
(was 68), `dispatch_size` (was 72), `sha256(dispatch_table)` (was 140).

## Artifact crate changes (`crates/rv32/artifact/src/format.rs`)

- `FORMAT_VERSION` 3 → 4, `ENTRY_ABI_VERSION` 1 → 2, `HEADER_BYTES` 172 → 132.
- New digest offsets: `CODE_DIGEST_OFFSET = 68`,
  `INITIAL_MEMORY_DIGEST_OFFSET = 100`. Remove `DISPATCH_OFFSET_OFFSET`,
  `DISPATCH_SIZE_OFFSET`, `DISPATCH_DIGEST_OFFSET`.
- Delete `DispatchEntry`, `build_dispatch_table`, `verify_dispatch_table`, and
  all dispatch constants (`DISPATCH_ENTRY_BYTES`, `DISPATCH_ENTRY_*_OFFSET`,
  `MAX_DISPATCH_ENTRIES`, `MAX_DISPATCH_BYTES`).
- `build_artifact(code, initial_memory)` — drop the `entries` param. Stamp
  `memory_bytes = MAX_MEMORY_BYTES`, `initial_memory_size = initial_memory.len()`;
  reject `initial_memory.len() > MAX_MEMORY_BYTES`. Body becomes
  `header || code || initial_memory`.
- `verify_artifact`: read `memory_bytes` and require `0 < memory_bytes ≤ MAX`;
  require `initial_memory_size ≤ memory_bytes`; canonical layout is
  `code_off == HEADER_BYTES`, `mem_off == code_off + code_size`,
  `mem_off + initial_memory_size == artifact.len()`. Drop the dispatch section,
  digest, and `verify_dispatch_table` call.
- Fingerprint: domain `…format.v3` → `…format.v4`; canonical section order
  `header|code|initial_memory`; drop every dispatch-related folded constant and
  the `max_dispatch_*` caps; replace
  `initial_memory_size_equals_memory_bytes` → `initial_memory_size_le_memory_bytes`
  and add `memory_bytes_le_max`. Re-pin `FORMAT_FINGERPRINT` to the recomputed
  value (discovered by building with a self-referential const, then pinned as a
  literal so the const assert and `fingerprint_is_pinned` test guard it).

## Lowerer changes (`crates/rv32/lower`)

- `elf.rs`: delete `parse_symbols`, `Symbol`, and the now-unused symbol/binding
  constants (`SHT_SYMTAB`, `SHT_STRTAB`, `SYM_BYTES`, `STT_*`, `STB_*`). Keep
  the section reader and the `e_entry == 0` / RISC-V / ET_EXEC checks.
- `lower.rs`: delete `discover_dispatch_entries` and `is_entrypoint_symbol`.
  `validate_sections` no longer takes `symbols`; drop the `_start` symbol lookup
  and the object/data-symbol-in-.text checks (stripped builds have no symtab —
  `e_entry == 0` plus the `.text` layout rules are the entry contract).
  Build the initial memory by copying allowlisted PROGBITS data sections at
  their vaddrs into a zero buffer, track `high_water` (max end of a copied
  section, 0 if none), and return the prefix `memory[0..high_water]`.
- `Lowered`: drop `entries`; add `memory_bytes: usize` (the 64 KiB address
  space). `initial_memory` is now the stored prefix, not a padded image.
- `metadata.rs`: drop `dispatch_entries` from `render_metadata` and
  `summary_json` and the `MetadataInput.entries` field. Report
  `memory_bytes = lowered.memory_bytes` (64 KiB) and add
  `initial_memory_bytes = lowered.initial_memory.len()`; `initial_memory_sha256`
  is over the stored prefix.
- `lib.rs`: stop re-exporting `DispatchEntry`.

## CLI changes (`crates/tools/cli`)

- `build.rs`: remove `.env("CARGO_PROFILE_RELEASE_STRIP", "false")` and the
  `CARGO_PROFILE_RELEASE_STRIP=false` entry in `rv32_build_flags`; drop
  `entries: &lowered.entries` from the `MetadataInput`.
- `error.rs`: refresh the `Rv32LowerFailed` hint (no longer about missing
  `_start`/entrypoint symbols; stripped builds are valid).

## Testing

- `crates/rv32/artifact/tests/fingerprint.rs`: re-pin the literal; the
  sensitivity test already perturbs the domain to the v4 string — flip it to
  perturb to v3 so it still asserts a *difference*.
- `crates/rv32/lower/tests/lower.rs`:
  - Delete the symbol/dispatch rejection tests (`rejects_missing_start`,
    `rejects_*entry_point*`, `rejects_duplicate_entry*`,
    `rejects_when_no_entry_points_present`, `rejects_object_symbol*`,
    `skips_*_hex_symbol`, `accepts_weak_binding_entry`,
    `rejects_start_*`, `rejects_symtab_size_not_multiple_of_entry`,
    `build_dispatch_table_rejects_out_of_range_pc`).
  - Update `lowers_a_minimal_contract`: no `entries`; `initial_memory` empty for
    a contract with no data sections; `artifact.len() == 132 + code_len`.
  - Update `copies_data_sections_into_the_memory_image`: prefix length is the
    section's high-water (`0x104`), bytes still land at `0x100`.
  - Update metadata / summary / cli-json tests: drop `dispatch_entries`; assert
    `memory_bytes == 64 KiB` and `initial_memory_bytes` present.
  - Update `build_artifact_*` tests to the `(code, initial_memory)` signature;
    replace `build_artifact_rejects_wrong_memory_size` with a test that a prefix
    `< MAX` and `== MAX` are accepted and `> MAX` is rejected.
  - Update digest-offset perturbation tests to the v4 offsets.
  - Add `lowers_a_stripped_elf` (no `.symtab`) proving symbols aren't required.
  - Add an artifact-size assertion: a contract with a small data section yields
    `initial_memory_size < memory_bytes` and a `.rv32a` smaller than the old
    full-image size (`132 + code + high_water`).

## Out-of-scope cross-repo notes

The fingerprint cross-check makes the cut atomic: a v4 artifact is rejected by a
v3 hogd and vice versa. hogd must re-pin `kArtifactFormatFingerprint`, drop the
dispatch parse/validate, seed `a0`/`a1`, and enter at PC 0 in the same release.
Those changes are tracked in the proposal, not here.
