# Gateway design

The gateway is the public JSON-RPC boundary in front of hogd's internal x23
services. It lets operators scale and protect public traffic separately from
validator execution. It keeps authentication, quotas, queueing, and deployment
policy out of transaction-validity rules.

For methods, types, and errors, see the
[JSON-RPC reference](../../crates/json-rpc/open-rpc-spec/snapshots/thruput-spec.md).

## Shared server, separate lifecycles

[json-rpc-server](../../crates/json-rpc/json-rpc-server/src/native/service.rs)
implements the public methods against backend traits. The production gateway
and the local node supervisor supply the adapters. Validation and public errors
stay the same in both; connection, recovery, and process policy can differ.

[hogd-backend](../../crates/x23/hogd-backend/src/lib.rs) decodes XDR results and
orders transaction-status reads. It owns no connections and no retry policy.
Gateway adapters own deadlines, admission, metrics, and reconnects. The
supervisor also owns a child process. Put new response mappings in
`hogd-backend`, so that one backend result has one meaning in both deployments.

Each backend service has one responsibility:

- `SchedGateway` admits transactions and serves scheduler reads.
- The indexer serves committed receipts, events, blocks, and transaction history.
- `DbGateway` serves committed state reads.
- `ViewGateway` executes sandboxed contract views.
- `SchedulerCoord` controls the coordinator, including local block driving. It is
  not a public submit endpoint.

The [runtime manifest](runtime-manifest.md) connects local tools to these
services. Gateway configuration sets the production topology.

## Views and simulation

A view executes against a discarded scratchpad. It reports simulated effects
and never commits them, so it serves wallet previews and read-style calls. It
does not replay history, and it does not predict the state that a later
transaction will see.

`assumedAuth` declares the authorization to assume. The server verifies no
signature, but normal contract checks run. `caller` identifies an optional
parent contract.

`simulateInvocations` runs independent calls against one committed snapshot.
Results keep input order. Calls do not see each other's simulated writes. If the
snapshot attempt fails, the request returns no partial results.

`minStateBlockHeight` is a freshness floor, not a historical-state selector. If
the snapshot is older, the request fails with `stateHeightUnavailable`. The
server does not wait for a newer block; the caller decides when to retry. In a
batch, the floor applies to the whole group.

Batch limits are operator policy, not consensus parameters. The shared server
checks call count, total calldata, and declared gas before dispatch. The tools
pass each call's gas, the remaining time, and a response-size budget to hogd. A
client timeout does not guarantee that backend work has stopped. `getServerLimits` reports the endpoint's limits. Limits can change
between requests. Clients must handle rejections, and must not split a batch
into separate snapshots without the caller's consent.

## Admission and transaction outcomes

A successful `sendTransaction` means that the authoritative mempool admitted the
transaction. It does not mean that the transaction was nominated, executed, or
committed. An error can carry admission evidence: not attempted, rejected, or
unknown. A lost reply leaves admission unknown. Do not treat unknown as a
rejection, and do not resubmit it as a new transaction.

The shared server's pre-checks give better errors for transactions that the
executor would reject. They are advisory; the executor is authoritative. They
use the chain's network configuration and protocol defaults, not local limits.
See [transaction validation](../../crates/tx-validation/src/lib.rs).

The gateway is the public admission-policy boundary. Transaction validity does
not establish fair access to proposal capacity. Rate limits, quotas, and
overload shedding belong here, and depend on the deployment. A bounded connection
pool is not a denial-of-service policy. Read the gateway configuration and
admission code for the controls that a deployment has.

### x23 data pools

[x23-transport](../../crates/x23/x23-transport/src/lib.rs) builds the shared
pools. The gateway multiplexes data connections, so socket count alone does not
limit backend work.
Admission limits, execution permits, response bounds, and deadlines protect
different resources. Cancellation frees local waiting capacity; it cannot roll
back work that a backend already accepted.

For the controls and their defaults, see
[GatewayConfig](../../crates/json-rpc/json-rpc-gateway/src/config.rs) or run
`hog-gateway --dump-effective-config`. The [lookup benchmark](lookup-bench.md)
separates successful work, overload, local waiting, and backend duration.

## Transaction status

`getTransactionStatus` combines scheduler and indexer reads. Indexed commitment
takes precedence. The
[status sequencer](../../crates/x23/hogd-backend/src/native/transaction/status.rs)
owns the fallback rules for a failed backend.

- `recentFailure` is an advisory indexer observation. It can describe a
  different attempt. It never permits resubmission.
- `unknown` requires a miss from both backends. It does not prove that the
  transaction never executed.

Reads across services are not one atomic snapshot. Clients keep a known
commitment; a later advisory status does not downgrade it. A receipt describes
committed execution, which can be a failed execution.

## Batching does not change transaction boundaries

A JSON-RPC array groups requests for transport. Its entries can run
concurrently. It is not an atomic transaction or an ordered program. Send
dependent operations separately, and wait for the state that they need.

Backend submit batching combines independent submissions into fewer scheduler
calls. Each signed transaction keeps its own admission outcome. A timeout or
cancellation cannot undo an admission. See the gateway
[submitter](../../crates/json-rpc/json-rpc-gateway/src/submitter.rs) and the local
[submit batcher](../../crates/node-supervisor/src/submit_batcher.rs).

## Method availability

`getVersionInfo` lists the methods that the endpoint serves. The standalone
gateway does not serve `dev_getPendingTransactionCount`; a local node does. A
backend outage is an error, not evidence that a method is unsupported.

## Committed block reads

The gateway and the local node read blocks and block transactions from the
indexer, not from the scheduler coordinator. `getBlockByHeight` returns `null`
for a height that the indexer has not recorded. An unavailable or corrupt index
is an error, not a missing block.

`getTransactionsByBlock` pages transaction references ordered by transaction
hash bytes, not by execution order. Pass cursors back unchanged. An empty page
does not prove that the block exists; read its metadata. Serving bounds can lag
behind `getChainTip`.

These reads need runtime block-query support and indexed history. A gateway
upgrade cannot create missing history. To qualify both adapters against a real
runtime:

```sh
cargo build -p thruput-json-rpc-gateway --locked
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
THRUPUT_GATEWAY_BINARY="$PWD/target/debug/hog-gateway" \
  cargo test -p thruput-node-supervisor --test real_block_queries --locked \
  block_queries_match_receipts_through_both_servers_before_and_after_restart \
  -- --ignored --exact
```

A runtime built from source can need `GROUNDHOG_DEVNODE_PROTOCOL` set to its
intro token.

## Wire format conventions

The public API projects XDR into JSON: `camelCase` fields, tagged unions,
decimal quantities, and byte strings typed by meaning.

- Identities (addresses, public keys, validator IDs, hashes, and signatures) use
  unprefixed base58.
- Other bytes (transactions, code, calldata, return data, storage, event topics,
  and cursors) use `0x`-prefixed lowercase hex. A raw topic stays hex even when
  its decoded value is an address.
- Integers of 64 bits or more use canonical decimal strings, so JavaScript
  parsers keep full precision. Smaller integers are JSON numbers.

Choose an encoding by the field's meaning, never by its length. Requests that
use the wrong encoding fail; there is no fallback. The projection lives in
[json-rpc-types](../../crates/json-rpc/json-rpc-types/src/lib.rs), shared by
servers and clients. The JSON-RPC reference gives each field's encoding.

## Event parsing

Backend adapters reject malformed events; they never pass them on as valid
public events. See the [decoder](../../crates/x23/hogd-backend/src/event.rs)
for the binary layout.

## Operating the gateway

See the [runtime-manifest quickstart](runtime-manifest.md) for local wiring, the
[observability guide](observability-smoke-test.md) for health and metrics, and
the [benchmark guide](bench-scenarios.md) for repeatable measurements. A healthy
status means that the configured probes passed. It does not prove that all
methods work or that a load level is safe.
