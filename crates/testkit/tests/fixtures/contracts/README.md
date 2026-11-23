# Testkit contract fixtures

Contracts that testkit's live tests deploy. They are test fixtures, not
examples.

| Fixture | Used by |
| --- | --- |
| `caller-counter` | `tests/contract_smoke.rs`: a per-caller counter for read-after-write checks |
| `block-clock` | `tests/contract_block_time.rs`: reports the block timepoint, to check that a contract view sees moved block time |

This is a separate Cargo workspace, with its own toolchain and SDK pin, so it
can build for the RV32 target. `scripts/smoke-testkit-local.sh` builds both
artifacts and runs the tests:

```sh
cd crates/testkit/tests/fixtures/contracts
thruput build -p caller-counter --target-dir "$TARGET_DIR" -- --locked
thruput build -p block-clock --target-dir "$TARGET_DIR" -- --locked
```

Keep `thruput-sdk` in `Cargo.toml` on the release that the thruput-tools
workspace pins. CI's dependency cooldown check covers only the root
`Cargo.lock`, so when you update this workspace's `Cargo.lock`, pin each crate
to the version in the root lockfile (`cargo update -p <crate> --precise
<version>`).
