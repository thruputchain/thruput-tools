# Brief 09 — subscription measurements and acceptance

## Conditions and scope

- Local Apple M5 Pro, 24 GiB RAM, macOS arm64 (Darwin 25.6.0), APFS-backed
  temporary archive directories. Rust 1.96.0, workspace 0.17.7, **debug** profile
  (`unoptimized + debuginfo`), jsonrpsee 0.25.1, protocol bindings 0.36.0.
  `RUST_LOG=error` for the measured server. No concurrent workspace build/test
  commands during the sampled intervals. Other host activity was not controlled.
- **No running hogd was used for these measurements.** A counting x23 test server
  using rpc_driver 0.11.0 implements procedure-10 cursor/parking/publication
  behavior. This measures the actual firehose binary, storage and WebSocket
  fanout; it does not measure C++ hogd cost or qualify the `sale` deployment.
- Payload source: Brief 08's `hundred-transactions.hex`, captured from
  hog-devnode **0.1.28-rc.1**, commit
  `d54945959a6a089e95e1337404ea71e5fcdc544c`, protocol `hogp/63c9284d8d5727e`.
  Each synthetic block repeats its **100 dev-wallet self-calls / zero emitted
  events**, changes height/parent and recomputes the consensus hash. Original
  transaction/event roots remain intact and the ingester verifies every block.
  These are replay fixtures, not newly executed application transactions.
- Each record is **27,600 raw XDR bytes**, or 55,202 characters including the
  `0x` hex prefix, plus the routing envelope and JSON-RPC frame. This run does
  **not** qualify the previously measured 272,400-byte / 1,000-transaction shape,
  multi-megabyte blocks, or a 10,589-event block.
- Driver: ignored integration test `measure_fanout_and_catchup`, **40 blocks per
  run**, 250 ms sleeps between synthetic publications plus `ps` sampling work.
  Actual runs lasted about 10.58 seconds (about 3.78 blocks/s). Server is a
  separate process; clients and the synthetic upstream run in the test harness
  on a four-worker Tokio runtime. Every client ACKs after receipt. Archive writes
  keep Brief 08's record fsync and directory fsync.
- Defaults: max 64 subscribers, 8 queued records / 2 MiB per live queue, 4 MiB
  maximum delivered record, two transport queue slots per connection, 5-second
  live ACK deadline, 10-second archive ACK deadline, 30-second lag progress window.

## Fanout cost and latency

CPU is the firehose process's `ps time` delta / wall time, expressed as a
percentage of **one core**; it includes ingestion, root verification, archive
writes and fanout. It excludes the client/upstream harness. CPU time has 10 ms
sampling resolution. RSS is sampled every publication interval, not an exact
high-water mark; memory columns are KiB. Each count starts a fresh process.

| Subscribers | CPU seconds / wall seconds | CPU, one core | RSS before traffic | Sampled peak RSS | Latency p50 | p95 | Max | Samples |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.61 / 10.5748 | 5.77% | 14,864 | 15,872 | 4.316 ms | 4.756 ms | 4.772 ms | 40 |
| 10 | 1.52 / 10.5765 | 14.37% | 15,312 | 18,304 | 5.631 ms | 7.689 ms | 8.290 ms | 400 |
| 50 | 5.01 / 10.5825 | 47.34% | 16,432 | 19,936 | 14.234 ms | 21.267 ms | 24.172 ms | 2,000 |

Latency starts at `ingestedAtUnixMicros`, recorded immediately after durable
append and before hub publication, and ends when the client has received and
JSON-decoded the notification, **before** sending its ACK. Both timestamps use
the same host clock. Every sample was below the driver's 250 ms block interval.
This is ingest-to-subscriber latency, not consensus-publication-to-ingest latency.

Every run saw exactly **82 upstream procedure-10 calls**: initial caught-up and
parked requests, then one caught-up request and one parked request per publication.
Maximum simultaneous handler calls was **one**. Changing subscriber count did
not add event-service calls. No subscriber demoted in these measured runs.

## Catch-up

Immediately after each live run, one additional subscriber read heights 1–40
from the same **warm** archive, ACKing each record, with the original connections
still attached and idle. Timing includes subscribe setup, disk reads, XDR decode,
hex serialization, WebSocket delivery, client byte validation and ACK round trips.

| Existing idle subscribers | Replay elapsed | Throughput |
| ---: | ---: | ---: |
| 1 | 0.194577 s | 205.57 blocks/s |
| 10 | 0.192318 s | 207.99 blocks/s |
| 50 | 0.195826 s | 204.26 blocks/s |

These are small warm-cache replays over loopback, not cold-disk or WAN throughput.
The ACK protocol adds one client round trip per block and can dominate replay on
a high-latency connection. There are no replay calls to hog-event.

## Slow subscriber and correctness evidence

The automated process-level test uses the actual firehose binary and a counting
x23 handler. It starts with zero, then one, then fifty subscribers; publication
continues in every phase. The handler asserts **procedure 10 on every request**,
records active call count and verifies that subscribing adds zero calls. Every
publication adds exactly two calls, independent of subscriber count.

Then a **51st** subscriber receives height 4 and withholds its ACK while heights
5–8 publish. Conditions: 8,624-byte bootstrap fixture records, a two-record live
queue, 1-second live/archive deadlines, synthetic publications driven by healthy
client completion. It demotes once on queue overflow; its queued records are
cleared. After ACKing 4 it receives height 5 from the archive, then stalls again.
It closes as `catchup_timeout` with **nextHeight 5**. All fifty healthy subscribers
receive every expected height through **12**, finish with nextHeight **13** and
lag **zero**. Ingestion archives all twelve blocks. The server records exactly
**26 procedure-10 calls**, maximum active calls **one**, and fifty remaining
subscribers. Continuity and call counts are assertions, not log observations.

Additional tests cover:

- Historical replay from height 1 through a populated archive; publish height 11
  while height 10 is awaiting ACK, then continue live without gaps or duplicates.
- Byte-for-byte equality between the archive and delivered records; feed received
  bytes into an independent destination archive to verify both roots, consensus
  identity and continuity using the existing fixture-tested verifier.
- Drop a connection after receiving height 12 without ACK; reconnect at 12,
  receive its documented idempotent repeat once, then height 13 once.
- Byte-cap demotion, count-cap rejection, oversized-record termination, live ACK
  timeout followed by demotion/termination, and a growing-lag archive subscriber
  whose individual ACK still meets its deadline.
- Connection-scoped ACK validation, future and invalid cursors, generated-client
  round trips, subscriber removal/reuse, active metric-label cleanup and exposure
  configuration validation. Existing recovery, corruption and procedure-10-only
  tests continue to pass.

## Design qualifications

The separate-process, single-reader, immutable archive, memory-tail/archive modes
and demotion policy remain as designed. Brief 08's one-file-per-height layout
and existing content-addressing difference are unchanged.

Three explicit refinements need review:

1. ACKs are part of the subscriber protocol. They bound outstanding work and make
   application stalls observable, at the cost of a client round trip per block.
2. Termination carries its cursor in a typed **subscription close notification**,
   not a WebSocket close frame. Multiplexed siblings keep working. A fully blocked
   or dead connection cannot be guaranteed to receive any final message; clients
   must retain a durable cursor. Terminal enqueue is bounded to 250 ms.
3. A configurable **4 MiB delivery ceiling** bounds subscriber memory. The ingester
   and archive retain their original protocol limits. Operators must raise the
   ceiling to deliver larger archived records; the terminal reason preserves the
   cursor rather than skipping a record.

No deployment, C++ hogd instrumentation, live hog-indexer mailbox test, release-build
capacity claim, or cold-cache/WAN benchmark was performed in this brief. The design
assigns live `sale`/devnet verification to the deployment owner. These local results
are reproducible acceptance evidence, not a replacement for that qualification.

## Reproduction and checks

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo test -p groundhog-firehose --test subscriptions \
  measure_fanout_and_catchup -- --ignored --nocapture
```

The first four checks passed; the measurement test also passed. Network/process
integration tests were run outside the filesystem/network sandbox so they could
bind loopback listeners. The benchmark prints machine-readable JSON rows with
raw timing, CPU, RSS and upstream-count measurements for each subscriber count.
