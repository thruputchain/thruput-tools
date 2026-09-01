# Transaction Ingress and Proposer-Local Races

This note captures the current `hogd` + `chuckd` behavior that matters for
RPC gateway design. It is based on a code review of the current trees, not on
the intended architecture alone.

## Bottom line

Groundhog's remaining game-theory pressure is concentrated at the proposer's
local ingress and proposal execution path, not at validator replay.

The gateway therefore needs to be designed first as an admission-control and
defense boundary, and only second as a convenience API or throughput relay.

For testnet v1, that does **not** mean we need the final form of ingress
policy on day one. It means v1 should establish the correct architectural
boundary so stronger policy can be added later without changing transaction
semantics.

## What the code actually does

### 1. Validators do not independently rerun the proposer race

This is the most important correction to the common mental model.

- `chuckd` asks the local scheduler to nominate a block in
  `sub/baton/baton/nominate.go`.
- `hogd` returns a nomination containing the winning transaction hashes plus
  per-transaction `NondeterministicProposalData`.
- Validators call `Schedulercoord_validate` on that exact nomination in
  `sub/baton/baton/vproposal.go`.
- `hogd` validation reconstructs `ProposedTransaction`s from:
  - the nominated tx hashes
  - the nominated nondeterministic data
  - the locally cached vetted tx bodies
- Validation then runs that nominated batch on `executor_idx=0` in
  `hogd/mempool/rpc_from_coord.cc`.

So the network is not exposed to "every validator races threads and picks a
different winner." Validators replay the proposer's chosen batch.

### 2. The proposer path is still timing-sensitive

`hogd` proposal execution is deliberately parallel.

- `schedulercoord_nominate()` schedules candidate txs out of the mempool.
- `schedule()` launches multiple proposal batches.
- `propose_batch_in_chunks()` splits work into many big chunks.
- `multiplex_large_chunk_across_executors()` further shards those chunks across
  multiple executors and TCP streams.

Conflicting `NNInt` / balance reservations are therefore still won by whichever
proposal worker reaches the relevant lock first. That race happens locally at
the proposer.

### 3. Validators do not currently verify a canonical scheduling policy

`hogd/mempool/rpc_from_coord.cc` contains an explicit TODO:

> Need to ensure that the node that proposed the block is following the
> expected scheduling policy.

That means validators verify the proposed result, but do not currently verify
that the proposer followed a canonical fairness policy while constructing it.

### 4. Transaction ingress in the current validator stack is already separated from hogd

Public submission today is not "internet -> hogd mempool".

The current path is:

1. client -> `chuckd` `/wallet`
2. `Wallet_submit()`
3. `UnvettedSubmitter`
4. `Schedulercoord_submit_unvetted()` into `hogd`
5. `hogd` auth-verifies and adds txs to the scheduler
6. `hogd` notifies `chuckd` when txs become vetted
7. `chuckd` disseminates vetted txs over Dogwood

So ingress is already separated from mempool internals.

## Review of common claims

### "Validators blindly accept the proposer's tx list"

Overstated.

Validators do not rerun proposer scheduling, but they also do not blindly
accept the tx list. They:

- check block height / previous block hash consistency
- reconstruct the proposed tx batch from tx hashes + nondeterministic data
- re-execute validation
- recompute the resulting block hash

What is true is narrower:

- validators replay the proposer's chosen batch
- validators do not currently enforce a canonical scheduling policy upstream of
  that batch

### "The proposer is an absolute dictator for their slot"

Directionally true for local tx selection, but too strong if read as a
consensus statement.

The proposer has broad control over which locally conflicting txs make it into
its proposal. But the resulting proposal still has to pass scheduler
validation and SCP consensus.

### "The only way to win a contested reservation is to be the first packet processed"

Directionally true, with an important qualifier.

The code supports a proposer-local timing race during proposal execution.
However, the exact pressure point is whichever public component first admits
transactions into the proposer's private path. In the current architecture that
is not `hogd`; it is the gateway tier in front of it.

### "This could overwhelm the proposer's NIC before txs reach the mempool"

Real concern, but this is a deployment and gateway-capacity statement, not
something directly proven by the code.

What the code proves is:

- there is a proposer-local ingress pressure point
- current public ingress code does not implement strong defenses

Whether that becomes a NIC bottleneck, relay queue bottleneck, or gateway CPU
bottleneck depends on deployment.

### "This is now a network ingress management problem"

Accurate.

The consensus-divergence risk from validator-local thread races is largely
avoided by proposer replay. The residual operational risk is concentrated at
public ingress and proposer-local admission.

## Gateway implications

### The public gateway is the primary defense boundary

The dedicated RPC gateway should own:

- authentication
- rate limiting
- dedupe
- bounded queues
- overload shedding / backpressure
- future QoS policy

If those controls are missing, the proposer-local race is still exposed, just
through a thinner or thicker relay stack.

For v1, the minimal useful set is:

- deterministic tx-hash dedupe
- basic rate limits
- bounded queues
- overload shedding
- observability around admission and backend latency

That is enough for a sensible testnet launch. Stronger QoS and quota systems
can come later.

### `hogd` vs `chuckd` submission is mostly a topology choice

Routing `sendRawTransaction` directly to `hogd` or indirectly through
`chuckd` changes:

- hop count
- queue placement
- private ACL shape
- where submit acknowledgements are produced

By itself it does **not** materially change the fairness posture. If `chuckd`
is only a relay, the same game still exists one hop downstream.

### The gateway should preserve one submit contract

Regardless of backend choice, external submit semantics should remain:

- success = admitted by the authoritative downstream mempool path, not
  nominated or committed
- `TxRejected` = the exact signed transaction was definitely rejected by that
  path
- retryable and internal admission outcomes = overload or backend failure, not
  proof of rejection
- transport failure = indeterminate admission, not rejection

The gateway should not silently change `sendRawTransaction` into "accepted by a
relay queue" when routed through `chuckd`.

## Commercial RPC operators

Longer term, the likely industry shape is that high-throughput users talk to
specialized RPC providers rather than directly to a validator-run public
endpoint.

That does not change the architecture here. It reinforces it:

- the gateway boundary remains the public edge
- validator internals remain private
- third-party operators can compete on reliability, indexing, auth, and QoS
- transaction validity stays separate from ingress policy

This is a later-stage ecosystem outcome, not a requirement for v1 testnet.

## Protocol / Rust generation status

The Rust protocol crates have moved forward enough that the gateway draft
should use them directly where possible.

### What exists now

- `groundhog-xdr` provides generated Rust XDR types with `ReadXDR` /
  `WriteXDR` derives.
- `groundhog_scheduler` provides generated x23 client/server bindings for
  both the internal `SchedulerCoord` service and the public `SchedGateway`
  service.
- `ParallelSignedTransactionBatch` and related wrappers already exist in
  `groundhog_scheduler`.

That means the direct-to-`hogd` submission path now has a concrete typed Rust
surface.

### Submission is direct-to-SchedGateway

The gateway submits directly to hogd's `SchedGateway` admission method over
that typed surface; it does not fall back to the internal coordinator service.
