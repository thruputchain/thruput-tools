# RV32 artifact design

The compiler and linker produce an ELF; the lowerer turns it into the bounded,
self-verifying artifact that Groundhog executes. The artifact crate owns the
Rust format implementation, and the lowerer uses it rather than maintaining
another writer or validator.

## Code and data have different address spaces

Groundhog's RV32 execution model separates instruction fetch from data memory.
A data virtual address can numerically overlap an instruction address without
overlapping the same memory. The lowerer must validate each address space on
its own terms. Treating the ELF as one flat code-and-data image would reject
valid layouts or carry a useless code-sized shadow in the initial data image.

The linker chooses data addresses. The lowerer cannot freely relocate them:
compiled load/store instructions already depend on those addresses. It can
store only the nonzero span and record where that span loads into guest memory.
Zero-filled regions are materialized by the runtime. This avoids serializing a
large empty prefix for stack or guard space.

The load offset has no stack policy of its own. Guest startup owns stack
placement. Keep linker, guest entry ABI, and runtime loader changes coordinated;
a locally valid file alone does not establish compatibility across those owners.

## Dispatch belongs to the guest

The guest entrypoint dispatches method identifiers, including unknown methods.
The artifact does not need a second host-side method table or an ELF symbol
scan. Stripped builds can therefore remain valid, and dispatch behavior is
ordinary guest execution subject to its execution budget.

## The format is an executable contract

Header layout, validation rules, digests, and format fingerprint live in
[format.rs](../../crates/rv32/artifact/src/format.rs). The runtime's independent
implementation must agree with that contract. Do not copy header offsets or
fingerprint values into a design document and treat them as another authority.

The lowerer validates the ELF, creates code and initial-memory sections, writes
the artifact, and self-verifies it. Acceptance must reflect the runtime's actual
loader rules, not merely whether an ELF parser accepts the input. Real compiler
outputs and malformed-artifact tests cover different parts of that boundary.

## Source map

- [ELF parsing](../../crates/rv32/lower/src/elf.rs)
- [Lowering and layout validation](../../crates/rv32/lower/src/lower.rs)
- [Artifact format, writer, and validator](../../crates/rv32/artifact/src/format.rs)
- [Standalone usage](../../crates/rv32/lower/README.md)

The SDK repository owns the guest linker script and startup code; `hogd` owns
the VM and loader. This repository owns their off-chain artifact boundary.
