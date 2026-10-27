# thruput-testkit

Test a Thruput application the way it is used: as several users acting at
once. A scenario runs a group of agents as ordinary async Rust, each with its
own deterministic account, against a local node the harness launches or a network
already running. Agents deploy contracts and call them through typed proxies
generated from the contracts' own interfaces, coordinate over a mailbox, and
assert on what they observe, so a test can check behavior that only appears
when users interact.

See the crate's rustdoc (`cargo doc -p thruput-testkit --open`, or
[`src/lib.rs`](src/lib.rs)) for usage and examples, and the
[testkit design notes](../../docs/testkit/DESIGN.md) for its ownership and
timing model.

The live tests in `tests/contract_smoke.rs` cover atomic initialization,
registration and hash-only deployment, calls, manual blocks, and recovery
across scenario runs. Run them with `scripts/smoke-testkit-local.sh`.
