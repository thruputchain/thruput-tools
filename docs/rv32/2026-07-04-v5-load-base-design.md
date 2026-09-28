# RV32 v5 Artifact Cut — initial-memory load offset (`initial_memory_start`)

Scope: `crates/rv32/artifact` and `crates/rv32/lower` in this repo. The hogd
C++ loader change is specified in hogd's own RV32 artifact v5
`initial_memory_start` design note (same date, same cut) — field names, fold names, and constants below are
aligned with that spec and must stay lockstep. Motivated by the SDK's move to
a stack-first guest layout (stack below the data sections, heap above them
growing up), which would otherwise put a stack-sized leading-zero shadow in
every stored image.

## Decisions

- **Decision 1 — `initial_memory_start`.** New u32 header field: the guest
  data VA where the runtime stages the stored image (purely a load offset —
  the VM derives no stack semantics from it). The runtime materializes data
  memory as `zeros[0, start) || initial_memory || zeros[start + size,
  memory_bytes)`.
- **Decision 2 — minimal-span storage.** The lowerer stores the minimal
  nonzero byte span of the composed data image (byte-precise on both ends),
  not the `[0, high_water)` prefix. An all-zero image stores as size 0,
  start 0. (hogd's spec suggests "lowest data-section VA"; byte-precise
  trimming is a strictly smaller valid choice and explicitly permitted — the
  field is free to move.)
- **Decision 3 — canonicality rules.** Loader/verifier require
  `start + size <= memory_bytes` (replaces the v4 `size <= memory_bytes`
  check), and `start == 0` when `size == 0`. No alignment requirement on
  `start`. Content-based trimming is *not* enforced on load.
- **Decision 4 — drop the stored file offsets.** v4's `code_offset` and
  `initial_memory_offset` were pinned by the verifier to the only values it
  accepted, i.e. pure redundancy. v5 removes both; section positions are
  implicit and the loader requires `header_bytes + code_size +
  initial_memory_size` to equal the file size exactly.
- **Decision 5 — entry ABI 3: `sp` seeded to 0.** The guest owns stack
  placement and sets `sp` in `_start`; a stack touch before that wraps below
  0 and traps OOB. (v2 seeded `sp` to the top of data memory, which under
  stack-first would point into the heap — the exact silent-corruption hazard
  this layout removes.)
- **Decision 6 — nothing else moves.** Syscall ABI (1), gas table (3), gas
  digest, magic, and the lowerer's null-guard rule (`data section addr >= 8`)
  are unchanged.

## v5 header layout (128 bytes, little-endian, no padding)

```text
  off  size  field
    0     8  magic "GHRV32A1"
    8     4  format_version          value 4 → 5
   12     4  header_bytes            value 132 → 128
   16     8  vm_profile_digest
   24     4  flags                   (0)
   28     4  entry_abi_version       value 2 → 3
   32     4  syscall_abi_version     (1)
   36     4  gas_table_version       (3)
   40     8  gas_schedule_digest
   48     4  memory_bytes            (65536)
   52     4  code_size
   56     4  initial_memory_size
   60     4  initial_memory_start    new (guest VA, not a file offset)
   64    32  sha256(code)            position was 68
   96    32  sha256(initial_memory)  position was 100
```

Body: `header || code || initial_memory`, positions implicit. Removed from
v4: `code_offset` (was offset 60) and `initial_memory_offset` (was 64).

## Fingerprint

Domain `…format.v4` → `…format.v5`. New folds: the
`header.initial_memory_start.{offset,size}` layout pair, plus rules
`initial_memory_start_plus_size_le_memory_bytes` and
`initial_memory_start_zero_when_empty` replacing
`initial_memory_size_le_memory_bytes`. Removed folds: the
`header.code_offset` / `header.initial_memory_offset` layout pairs and the
now-vacuous `sections_must_be_in_bounds` / `sections_must_not_overlap` rules.
`canonical_section_order` and `no_section_gaps_or_trailing_bytes` stay (they
carry the derived-layout rule). The `entry_abi_version` value fold moves
2 → 3. Pinned value: `0x8e5c_af3e_9cd2_3b22` — hogd re-derives its
`kArtifactFormatFingerprint` independently and the two must match
(cross-check via `rv32-lower --print-format-fingerprint`).

## SDK follow-ups (out of this repo)

- `groundhog-rv32.ld`: stack-first section ordering.
- `_start`: already sets `la sp, __stack_top` per hogd's spec — required
  under entry ABI 3 since the VM no longer provides a usable `sp`.
