# Firehose store and serve (experimental)

Two independent processes share an immutable archive directory. The sole writer,
`hog-firehose-store`, uses one `EventIndexer1` connection and one outstanding pull
regardless of subscriber count. `hog-firehose-serve` owns WebSocket subscribers.
The gateway remains a separate process with its existing surface. Experimental wire
types and generated jsonrpsee client/server traits live in this unpublished crate.

```sh
cargo build -p thruput-firehose
hog-firehose-store discover localhost:9001 localhost:9002 localhost:9003
hog-firehose-store tail --event-endpoint localhost:9002 --store ./archive \
  --tip-endpoint localhost:9004 --listen 127.0.0.1:9550
hog-firehose-serve --store ./archive --listen 127.0.0.1:9552 \
  --ws-listen 127.0.0.1:9551 --poll-interval-ms 200 --tip-stale-seconds 60
hog-firehose-store verify --store ./archive
```

See [the subscription protocol and backpressure policy](subscriptions.md) for
subscribe/ack/resume, bounded queues, limits and metrics, and
[Brief 09 measurements](subscription-measurement-2026-09-18.md) for fanout evidence.
Serve listeners default to loopback. Non-loopback binding requires
`--allow-remote` and emits an unauthenticated-service startup warning.
It also defaults to one subscription per connection and logs that choice;
`--max-subscriptions-per-connection` explicitly overrides the limit. Local use
without `--allow-remote` retains the global subscriber cap as its per-connection
default. Pass `--allow-remote` for public proxy deployments even on loopback.

Store `run` ingests without block summaries. `tail` emits height, emitted
event count, transaction count, XDR payload bytes, and elapsed time since the
previous newly stored block. Its first interval starts when ingestion begins.
`--json` produces JSON lines for tail and a single discovery report; JSON tail also includes the
consensus, event-log, transaction-body and archive byte counts. Diagnostics go
to stderr. `verify` emits a JSON recovery report. Configuration is through the
CLI flags shown above; `RUST_LOG` controls tracing. The required event address
is never inferred. Both DNS hostnames and numeric socket addresses are accepted.
`--protocol` can override the compiled, exact-width protocol token; it cannot
make incompatible XDR schemas compatible.

## Directory interface and failure isolation

Store exclusively holds `LOCK`. Serve never locks or writes the directory and never
opens a `Store`. It discovers the maximum fixed-width height filename at startup
without reading record contents, then polls the next height every 200 ms.
If initially empty, it rescans until records appear and starts publication at the
minimum height. Existing history at startup is served from disk without republication.
Each new record is decoded and verified once before publication to the existing
bounded fanout. Atomic publication means readers see a complete record or no file.
There is no IPC or dependency on writer reachability.

Stopping store leaves subscribers connected with their cursors intact; restarting
store resumes publication through the directory. Restarting serve does not interrupt
ingestion. Tail read or verification failures do not advance its tip.

Serve exposes health and metrics on `127.0.0.1:9552`, separately from store's
`127.0.0.1:9550`. Health uses the latest record's modification time, including at
restart. Above `--tip-stale-seconds` (default 60), `/health` returns 503 and
`firehose_healthy` is zero. `firehose_tip_age_seconds` reports age; it is NaN before
any record. An empty archive is healthy startup until a first record arrives.
Store health continues to describe its upstream connection.

## Discovery and protocol safety

Bindings use the workspace's pinned protocol-rs release.
`EventIndexer1`, program `0x4321aaac` version 1, has **no separate intro
procedure**. Its procedure-10 request contains the protocol token. `discover`
sends an uninitialized, non-parking pull as the program/protocol probe. This may
read one block but changes no server cursor. Program/version/protocol mismatch
is a clean negative. Discovery prints one line per candidate, matches first,
with `EventIndexer1 (matched)`, `no match`, or `probe error` and a short reason.
Transport diagnostics are suppressed only for discovery. After a failed x23
probe, a bounded HTTP HEAD check can confirm an HTTP listener as a clean negative;
unconfirmed connection failures and timeouts remain probe errors.

Exit codes (also in `discover --help`): **0** if any candidate matches, even if
others fail; **1** if none match and all probes answer cleanly; **2** if none
match and at least one probe fails (inconclusive). `discover --json` emits
`{"match":"127.0.0.1:9002","candidates":[{"address":"127.0.0.1:9002","outcome":"matched","reason":null}]}`.
Candidate outcomes are `matched`, `no_match`, or `probe_error`.
The top-level `match` is the first matched address, or JSON `null`; reasons are
strings for negatives/errors and `null` for matches. All candidates are retained.

An unready outbox
still identifies the service. Discovery opens and drops one candidate connection
at a time.

**Procedure 11 is never called.** It drains hog-event's transaction-failure
mailbox and would steal hog-indexer's diagnostics. The private generated client
is wrapped in a type exposing only `pull`; recording-sender and wire-server
tests assert procedure 10 for every request, including probes and parked pulls.

The observed hog-devnode readiness/runtime manifest has no event endpoint, so
`HogdDevEndpoints` is unchanged. Productionisation requires **hogd to advertise
the event endpoint in readiness and the supervisor to carry it into the runtime
manifest**. That is a separate upstream change. An operator can obtain candidate
ports from `lsof -nP -a -p <hog-devnode-pid> -iTCP -sTCP:LISTEN`, then explicitly
pass those addresses to `discover`.

## Ingestion and restart

Each connection starts with durable `Initialized` progress (or `Uninitialized`
for a new archive). A `CaughtUp` response adopts the origin and changes the next
request to the 0.36.0 **request-only `CaughtUp` cursor state**, which parks until
publication. This state is never persisted. After a block the client asks with
`Initialized` again. There is no polling timer or deadline on the parked pull.

All outcomes have distinct logs and counter labels:

| Outcome | Action |
| --- | --- |
| Block | Verify roots/identity/continuity, fsync, then advance the cursor |
| Replay | Identical archived record; count `replay` and back off one second |
| CaughtUp | Healthy normal state; adopt epoch boundary and park |
| NotReady | Degraded health; retry after one second |
| MismatchedProtocol | Fail closed and exit |
| ProgressConflict | Fail closed and exit; never reset the archive |
| InternalFailure | Degraded health; retry the durable cursor after one second |
| Unknown | Fail closed and exit |

Transport failures reconnect with a one-second backoff and the durable cursor.
The decoder uses the replay protocol's 31-bit message limit rather than the
gateway helper's 64 MiB limit. This experimental service assumes a trusted,
private hogd endpoint; its memory requirement includes an entire decoded block
and temporary verification/encoding buffers.

SIGINT/SIGTERM cancels the parked pull and drains HTTP requests. A store failure
stops ingestion. SIGKILL may leave one unpublished temporary record; recovery
ignores it. A published block is already the cursor, so replay cannot create an
extra height. An identical serialized repeat is a no-op. Conflicting content,
height gaps, parent-hash mismatches, and discontinuous epoch origins are errors.
No rollback, rewrite, pruning, or repair mode exists.

## Storage choice and audit

The directory itself is the height index: `00000000000000000001.xdr`, etc.
Each file contains the exact XDR `EventIndexerPulledBlock`, including origin,
consensus value, both batches, and identity. A held advisory `LOCK` permits only
one writer/verifier per archive. An initial caught-up origin on an empty archive
is stored in `origin.xdr`. Subsequent cursors are reconstructed from verified
blocks; epoch-only transitions can be rediscovered after restart.

Publication writes a fresh `pending` inode, calls `sync_all`, hard-links it to
the final filename without overwrite, syncs the directory, then unlinks
`pending`. A leftover temporary name is always unlinked before reuse, since a
crash can leave it hard-linked to a committed record. This requires a local
filesystem supporting hard links, directory fsync and advisory file locks.

This is simpler than a database or segment journal for this experimental slice:
one atomic record contains both data and cursor, with no separate index to
repair. The tradeoff is one inode and filesystem allocation unit per block,
plus an O(N) directory scan and full verification at startup. The measurements
below quantify that cost. A segment format is the next storage optimisation if
file count/allocation or recovery duration becomes material. There is no claim
that this layout has been qualified for millions of files.

Recovery verifies every consensus hash and recomputes both `events_hash` and
`txs_hash` using hogd's prefix Patricia-trie encoding, including transaction/event
pairing, nondeterministic proposal data, and duplicate-key rejection. Hashing the
whole serialized batch would be wrong. Live-node fixtures cover both one and
100 transactions, branch/extension/leaf paths, and order independence. Recovery
also validates file heights and chain/epoch continuity; corruption is an error,
never silently skipped or repaired.

As in hogd, signature wrappers are outside both consensus payload commitments;
these checks do not authenticate or detect changes confined to those wrappers.
Nor are consensus hashes an external trust anchor against an attacker rewriting
an entire archive. This verifies consistency with the archived commitments.

The design note proposed content-addressed payload files and a height index.
This slice keeps payloads in immutable height files with their content hashes,
without a second content-addressed file layout or deduplication layer.

## Observability

Store `/health` returns 200 for a working ingester, including when parked, and 503
before its first successful response or while retrying backend failures.
An unexpected WebSocket server stop shuts down serve with an error.
`/metrics` uses a private Prometheus registry:

| Metric | Meaning |
| --- | --- |
| `firehose_blocks_ingested_total` | Newly durable blocks in this process |
| `firehose_bytes_ingested_total` | New full archive record bytes |
| `firehose_events_per_block` | Emitted-event histogram |
| `firehose_current_height` | Durable contiguous height, or -1 before an origin |
| `firehose_tip_lag_blocks` | Sampled scheduler tip minus archive height |
| `firehose_tip_age_seconds` | Serve tip age; NaN before its first record |
| `firehose_healthy` | Process health, including stale-tip degradation on serve |
| `firehose_pull_seconds` | Pull duration histogram, including parking |
| `firehose_parked_seconds_total` | Gauge of accumulated parked seconds, including the current pull |
| `firehose_store_bytes` | Logical durable record bytes, including recovered records |
| `firehose_store_allocated_bytes` | Allocated record bytes on Unix; logical bytes elsewhere |
| `firehose_pull_outcomes_total{outcome}` | Pull outcomes, including identical replays, unknown values and transport failures |

Pull replies contain no tip height. For independently measured lag, supply
`--tip-endpoint` with an explicit `SchedGateway` address. A separate serial
scheduler client negotiates its intro and samples the tip every 250 ms; it never
adds an event-service connection. Without this source, or during sampler errors,
lag is **NaN**, not a misleading zero. The sampled gauge can miss short spikes
and reflects observations from two independently moving services. Allocation
metrics exclude directory metadata and the lock/temporary file.

## Reproducible local measurements

See [the measurement report](measurement-2026-09-18.md) for conditions, measured
latencies, growth estimates, restart evidence, and design differences.

The examples are **local-node test drivers**, not production coordinators:

```sh
cargo build -p thruput-firehose --examples
# Mirrors node-supervisor's proposal/commit and compatibility hash sequence:
target/debug/examples/drive_blocks <scheduler-coord> <archive> 30 250
# Same native wallet self-call shape as the workspace benchmark; unique nonce start:
target/debug/examples/submit_traffic <runtime-manifest.json> 100 100000 traffic-fixture-v1
target/debug/examples/drive_blocks <scheduler-coord> <archive> 5 250
```

For local schema-4 nodes, the example owns account zero derived from
`thruput-firehose:traffic-fixture-v1`. Supply that fixture's public allocation
before starting the node; the manifest cannot supply a signer. Hosted schema-3
manifests retain the separate bootstrap adapter when the seed argument is omitted.


The block driver reports elapsed commit-RPC time and commit-request-to-file
visibility, polling the archive filename at approximately 1 ms. It fails a
measurement after five seconds without file visibility. File visibility can
precede the final directory fsync, so this is an arrival measurement, not a
power-loss durability timestamp. All records are fsynced before the ingester
advances its cursor or emits its tail line.
