# Testkit design

`thruput-testkit` runs concurrent application actors as ordinary Rust tests.
Each actor can make calls, wait for results, coordinate with other actors, and
assert on what it observes. This is useful when correctness depends on several
users interacting, rather than on one contract call in isolation.

## Relationship to other test tools

`thruput test --integration` provides the single-project build/deploy/test
loop. Testkit lets a small group of actors execute arbitrary application logic
and make concrete assertions. It is not a load generator or a cloud launcher.

The scenario owns setup, actor lifetimes, and result collection. It uses a
network handle for chain access. A local network owns an ephemeral `hog-devnode`
through the node supervisor; an existing network borrows a running endpoint.
Before a scenario uses a live network, it checks that the endpoint serves the
network hash named in its runtime manifest, so transactions are never signed for
a different network. An offline network supports coordination tests without a
chain. Transaction preparation, signing, and submission go through
`thruput-client`; testkit adds its own account, nonce, and receipt policy on
top. The owner of
a local target must finish cleanup even when an actor fails.

## Actor identity and transaction boundaries

Actors receive deterministic native test accounts. Funding is explicit so test
setup does not hide network writes or assume an empty account can submit work.
Contract registration, deployment, calls, and receipt observation remain distinct
operations. Deployment can fund and initialize the contract atomically.

This makes failures diagnosable: preparing an invocation is not submission,
submission is not commitment, and an observed receipt may describe failed
execution. Tests should assert on the stage they actually need. Preserve the
canonical transaction hash on ambiguous submission rather than retrying with a
new identity and risking duplicate effects.

## Reproducibility has limits

Seeded identities and actor randomness make inputs repeatable. The logical clock
coordinates actor code; it does not make OS scheduling, RPC latency, or an
external node deterministic. Local manual block production lets tests stage
transactions before requesting a block, but it does not promise an arbitrary
ordering of concurrent transactions inside that block.

Use actor messaging and explicit observations to establish required ordering.
Do not use wall-clock sleeps as proof that another actor's write committed.
Scenario completion propagates actor errors to the ordinary Rust test result.

## Where to read next

Start with the [crate Rustdoc](../../crates/testkit/src/lib.rs) and
[tests](../../crates/testkit/tests/) for API usage.

For a real local contract smoke, use
[scripts/smoke-testkit-local.sh](../../scripts/smoke-testkit-local.sh). The
script builds the caller-counter fixture and runs the live tests. Source tests
and a successful local run do not prove compatibility with every deployed
network.
