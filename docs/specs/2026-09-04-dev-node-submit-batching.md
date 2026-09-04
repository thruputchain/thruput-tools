# Dev-Node Transaction Submission Batching

**Status:** Active implementation spec
**Baseline:** `groundhog-rpc` `origin/main` (`b3cf718` when implementation was finalized)
**Scope:** Embedded JSON-RPC backend used by `groundhog node`

## Summary

Keep the public `groundhog_sendRawTransaction` API unchanged, but combine concurrent calls into larger internal `ParallelSignedTransactionBatch` submissions before forwarding them to hog-devnode's scheduler.

Today, the embedded dev-node backend creates one internal batch and one x23 call for every public transaction. Under sustained load, this path admits about 5,000 transactions per second even when the client sends many concurrent JSON-RPC requests. The production gateway already batches the same public method; the embedded backend should provide equivalent batching behavior.

## Goals

- Preserve the existing public JSON-RPC method, parameters, result, and errors.
- Preserve the rule that RPC success is returned only after the authoritative scheduler call succeeds.
- Pack concurrent submissions into valid multi-transaction `ParallelSignedTransactionBatch` values.
- Bound memory usage and reject excess load with `RpcServerError::Overloaded`.
- Respect request deadlines and avoid delivering replies to canceled requests.
- Keep chain-tip and read-plane access responsive while submissions are busy.
- Improve internal scheduler batch size and end-to-end admission throughput under load.

## Non-Goals

- Add a public bulk-submission RPC method.
- Change the XDR or hogd scheduler protocol.
- Change hogd authentication, scheduler locking, or commit behavior.
- Promise that every submitted transaction is semantically accepted. The current internal `submit_unvetted` response remains `void` and cannot report per-transaction rejection.
- Refactor the production gateway's batching implementation in this first slice.

## Current Behavior

`HogdDevSchedulerBackend::submit_raw_transaction` currently:

1. Wraps one validated transaction in a `ParallelSignedTransactionBatch`.
2. Calls `schedulercoord_submit_unvetted` and waits for it to return.
3. Triggers on-demand block production when configured.
4. Returns success to the public RPC caller.

A JSON-RPC request containing many `sendRawTransaction` entries does not become one scheduler batch. jsonrpsee executes those entries sequentially, and the embedded backend forwards each entry separately.

## Proposed Design

### Node-Supervisor Submit Batcher

Add a node-supervisor module responsible for queued scheduler submission. `HogdDevSchedulerBackend` owns a cloneable handle to this batcher while retaining its scheduler client for chain and read backends.

Each queued item contains:

- Validated transaction bytes.
- A one-shot response sender for the original RPC call.
- The request deadline, when one exists.

The batcher uses one bounded Tokio channel and one scheduler batch in flight. A single worker:

1. Receives the first item.
2. Drains all items currently available, up to the configured maximum batch size.
3. Removes canceled and expired items.
4. Packs one XDR segment per transaction: `segment_tokens` contains `1` for each transaction, `segment_bytes` contains each encoded length, and `xdr` contains the concatenated transaction bytes.
5. Makes one `schedulercoord_submit_unvetted` call.
6. Fans the shared scheduler result out to each live request.
7. Triggers on-demand block production once after a successful internal batch.

There is no time-based collection delay. At low load, one transaction is submitted immediately. Under load, requests accumulate while the previous scheduler call is in flight and are drained together on the next iteration.

### Defaults

- Maximum internal batch size: 1,000 transactions.
- Ingress queue depth: 10,000 transactions.
- Maximum internal batch payload: hogd's 64 MiB transport limit minus 64 KiB for XDR and RPC framing.
- Maximum outstanding transaction payload: 64 MiB across the queue and in-flight batch.
- Scheduler batches in flight: one.
- Backend scheduler timeout: 30 seconds.
- Maximum deadline skew within one internal batch: one second.

One in-flight batch matches hogd's currently serialized admission path and maximizes coalescing. Requests with materially different deadlines are submitted as FIFO-preserving cohorts so one nearly expired request cannot cancel newer work. Count and byte limits bound both retained memory and x23 message size. These values are private implementation constants for the first slice, not new CLI or public configuration.

### Backpressure And Errors

- A full ingress queue or exhausted byte budget returns `RpcServerError::Overloaded` immediately.
- A request that expires while queued returns `RpcServerError::BackendTimeout`.
- A scheduler transport or protocol failure is classified with the existing submission error mapping and returned to every live request in that internal batch.
- Dropping the batcher closes the queue, stops its worker, and resolves waiting requests with `BackendUnavailable` rather than leaving them blocked.
- Canceled callers are removed before packing where possible. Cancellation after submission does not cancel the shared scheduler call needed by other requests.

### Code Reuse Boundary

The production gateway already has a mature adaptive batcher in `crates/json-rpc/json-rpc-gateway/src`. That implementation is coupled to gateway-specific metrics, reconnection state, deadlines, and `GatewayError`, and the gateway crate is a non-published binary crate. Making node-supervisor depend on it would violate package boundaries; extracting it in this performance fix would also modify the production path unnecessarily.

The dev-node implementation will follow the gateway's tested batching and packing rules. A later cleanup may extract a shared generic primitive after both call sites have stable requirements.

## Testing

Add focused node-supervisor tests for:

- One low-load transaction produces one scheduler batch.
- Concurrent submissions are combined into fewer scheduler calls.
- Packed segment metadata and concatenated bytes preserve transaction boundaries and order.
- Batches do not exceed maximum transaction or byte counts.
- A full queue returns `Overloaded`.
- Expired and canceled requests are not submitted.
- Scheduler failures fan out to every request in the batch.
- Dropping the batcher resolves pending requests.
- Successful batches trigger on-demand block production.
- Chain-tip calls remain usable while submission work is queued.

Run:

```bash
cargo fmt --check
cargo test -p groundhog-node-supervisor
cargo clippy -p groundhog-node-supervisor --all-targets -- -D warnings
```

Then build the `groundhog` CLI from this branch, run it against the installed compatible hog-devnode, and repeat the 100,000-transaction shared-counter benchmark.

## Acceptance Criteria

- No public JSON-RPC or OpenRPC schema changes.
- Existing local-node tests continue to pass.
- Concurrent public submissions produce multi-transaction internal scheduler batches in tests.
- Queue, timeout, cancellation, and shutdown behavior are deterministic and bounded.
- The local benchmark completes with exact final-state verification.
- The benchmark records a measurable improvement in Groundhog RPC acceptance throughput, or produces evidence that hogd admission is now the limiting stage.

## Follow-Up Boundary

If scheduler batching does not materially improve admission throughput, instrument and optimize hogd separately. Likely follow-up work would reduce the time spent holding hogd's global admission lock, but that is outside this `groundhog-rpc` change.

## Validation Results

Initial performance validation used `hog-devnode 0.1.26` and a release build based on `groundhog-rpc` `v0.11.1`. The final implementation and automated tests were rebased onto `origin/main`; current-main real-node results are recorded in the pull request.

- 10,000 shared-counter transactions completed exactly. Successful throughput increased to 7,264 operations per second and the workload used two blocks instead of the previous five-block result near 4,000 operations per second.
- At 100,000 transactions and concurrency 64, RPC acceptance increased from about 4,800 to 16,555 transactions per second. Blocks contained as many as 18,560 transactions, confirming that the old roughly 2,500-per-block result came from singleton admission.
- The same 100,000 run committed 98,899 transactions while the `void` scheduler RPC reported success for all 100,000. Concurrency 128 increased acceptance to 18,079 transactions per second but increased the missing count. Final state matched committed receipts, so this is not receipt polling loss.

The first-slice performance goal is met, and the result identifies hogd's silent per-transaction rejection or proposal-failure handling as the next limiting stage. Before presenting scheduler RPC completion as transaction acceptance, hogd metrics must distinguish authentication failures, proposal execution failures, HOG discards, and pending transactions. A typed per-transaction scheduler result remains follow-up protocol work.
