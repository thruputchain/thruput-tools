# Experimental WebSocket firehose

`hog-firehose-serve` starts the subscription listener at
`ws://127.0.0.1:9551`; serve health and Prometheus use `http://127.0.0.1:9552`.
There is no authentication. `--allow-remote` is required if **either** listener
binds outside loopback, and logs a startup warning. No gateway changes or
additional event-service connections are involved.

With `--allow-remote`, subscriptions per connection default to **1**, even when
binding loopback behind a public proxy. Startup logs report:
`--allow-remote defaults --max-subscriptions-per-connection to 1; set it explicitly to override`.
Without `--allow-remote`, the default remains `--max-subscribers`, allowing
independent tip and backfill cursors on one local connection. An explicit positive
`--max-subscriptions-per-connection` overrides either default. Public reverse-proxy
deployments must pass `--allow-remote` even if the backend listener is loopback.

Exceeding this connection limit returns JSON-RPC error code **-32006**,
`Too many subscriptions on the connection`, with data `"Exceeded max limit of 1"`
(using the configured limit). Existing subscriptions and the connection stay open.
Clients can unsubscribe an existing cursor before retrying. With the public
default, an edge limit of two connections per IP permits at most two subscriptions
per IP; the global subscriber cap still applies independently.

The source of truth is `FirehoseApi` in this crate's `src/api.rs`, with
generated `FirehoseApiClient` / `FirehoseApiServer`. Wire types are in
`src/types.rs`. This experimental namespace is independent of
the gateway's versioned OpenRPC surface; the gateway does not register it.

## Subscribe, consume, resume

Subscribe from an **inclusive** height:

```json
{"jsonrpc":"2.0","id":1,"method":"firehose_subscribe","params":[{"from":"height","height":1}]}
```

Or from the next publication after the subscription is registered:

```json
{"jsonrpc":"2.0","id":1,"method":"firehose_subscribe","params":[{"from":"tip"}]}
```

The response result is jsonrpsee's subscription ID. It is used with
`firehose_unsubscribe`, and is distinct from the numeric `subscriberId` used
in acknowledgements and metrics. Each `firehose_block` notification contains:

```json
{"jsonrpc":"2.0","method":"firehose_block","params":{"subscription":123,"result":{"type":"block","subscriberId":1,"block":{"schemaVersion":2,"height":42,"consensusHash":"11111111111111111111111111111111","byteLength":27600,"record":"0x...","ingestedAtUnixMicros":1789720800000000}}}}
```

`record` is the **exact complete stored `EventIndexerPulledBlock` XDR**, transported
as canonical `0x` hex, not decoded JSON fields. `byteLength` counts original bytes,
not hex characters or JSON framing. Decode these bytes with the protocol bindings
to verify the consensus hash and transaction body hashes, and reject duplicates. The
envelope hash is the same commitment contained in the record. Hex doubles payload
size; it does not change the audited bytes. `ingestedAtUnixMicros` is captured
after durable append, before fanout; it is absent for archive reads and is not
part of the archived commitment.

After **durably processing** a block, acknowledge it on the same WebSocket:

```json
{"jsonrpc":"2.0","id":2,"method":"firehose_ack","params":[1,42]}
```

The result is `null`. There is one outstanding record per subscription. Wrong
heights, duplicate ACKs, unknown subscriber IDs, and ACKs from another connection
are rejected. Independent subscriptions can share a connection.

Persist the next desired height with application progress. To resume after
processing height 42, subscribe at 43. If processing or ACK delivery was uncertain,
subscribe at 42 and idempotently apply it again. The service makes no exactly-once
claim across connection loss; it cannot know whether a lost ACK was processed.
Within a subscription, every height is sent once and in order, including across
archive/live transitions. Future heights are allowed and wait for publication.
Missing historical heights terminate with `archive_error`; the server never
silently skips unavailable history. Height must be nonnegative and below `i64::MAX`.

Serve rejects a height below `max(0, tip - request_window)` at subscription attach
with JSON-RPC error code `-32009` and message `below_window`, before accepting the
subscription or delivering any records. This policy applies even when the record
is still on disk. `--request-window` is in blocks, default 42,428 (about 24 hours
at the measured 2.036-second cadence). Blocks give an exact height boundary without
estimating a changing cadence. The boundary itself and future heights are accepted;
`Cursor::Tip` is unaffected. Accepted subscribers keep their exact cursors when the
window advances and may finish catch-up under the existing deadlines. Missing or
unreadable records still terminate with `archive_error`; no history is skipped.

A server-initiated termination sends this final **subscription close notification**:

```json
{"jsonrpc":"2.0","method":"firehose_block","params":{"subscription":123,"result":{"type":"closed","reason":"catchup_timeout","nextHeight":43}}}
```

Clients must stop consuming this subscription on `type: "closed"`. The cursor is
inclusive and refers to the first unacknowledged height. This is an application
close notification, **not an RFC 6455 close frame**: jsonrpsee supports multiplexed
subscriptions, and terminating one must preserve the others. It gets a bounded
250 ms transport-enqueue opportunity. A dead or fully blocked connection can lose
any final message; in that case use the client's durably saved cursor. jsonrpsee's
generic client will not interpret this application notification automatically;
handle the generated `Notification::Closed` variant.

## Backpressure and memory bounds

| Flag | Default | Meaning |
| --- | ---: | --- |
| `--request-window` | 42428 | Maximum depth below tip for new height requests, in blocks |
| `--max-subscribers` | 64 | Global active subscription cap; also bounds connections |
| `--max-subscriptions-per-connection` | 1 with `--allow-remote`, otherwise `--max-subscribers` | Active subscription cap on each WebSocket |
| `--subscriber-queue` | 8 | Maximum queued live records per subscriber |
| `--subscriber-queue-bytes` | 2097152 | Maximum raw bytes in each live queue (2 MiB) |
| `--max-record-bytes` | 4194304 | Largest deliverable raw record (4 MiB) |
| `--live-timeout-ms` | 5000 | Live send plus application ACK deadline |
| `--catchup-timeout-ms` | 10000 | Archive read deadline; separately, send plus ACK deadline |
| `--catchup-progress-ms` | 30000 | Disconnect when archive lag grows across this window |

Limits must be positive. Record limits must fit the JSON framing bound. Oversized
records terminate with `record_too_large` and the resumable height; ingestion and
archive verification still accept the upstream protocol's existing bounds.
Increase the delivery limit explicitly to replay such a record.

On live queue overflow (count **or** bytes), publication immediately clears that
subscriber's queue and demotes it. The queue never grows. New publications merely
wake a subscriber in catch-up mode; that subscriber sequentially reads its next
height from the immutable archive. The existing outstanding block is not resent.
An ACK at the archive tip changes back to live mode under the same short lock
that publishes records. Publication before that decision is read from disk;
publication after it enters the live queue. This prevents both gaps and duplicates.

A live send/ACK deadline also demotes. Its existing send/ACK operation gets one
additional catch-up deadline, then terminates as `catchup_timeout`. Already
catching-up subscribers get only the catch-up deadline. Catch-up reads that exceed
their deadline terminate as `archive_timeout`; failed reads as `archive_error`.
Each archive record is re-verified before delivery; decode or commitment failures
terminate as `archive_corrupt`.
Even if each ACK meets its deadline, growing lag over a progress window terminates
as `catchup_falling_behind`. Equal or decreasing lag is allowed, including long
historical replays. Time spent idle at the tip does not count as catch-up time.

Live records are shared `Arc`s, with one bounded queue per subscriber. Archive
readers keep one record at a time. There are no prefetch tasks, unbounded channels,
retained historical subscriber labels, or subscriber-controlled upstream calls.
The JSON-RPC transport has two queued messages per connection, a 4096-byte request
cap, and a response cap of `2 * max_record_bytes + 4096`. Memory also includes the
current raw/hex record per subscriber, serialization buffers, transport/kernel
buffers and the unchanged ingester's decoded block/verification buffers. Queue
bounds alone are not a whole-process RSS cap. At the maximum accepted record size
and subscriber count the bounded worst case is much larger than the smoke RSS;
size subscriber and record limits together for the host.

Ingestion never waits for subscriber reads, sends, ACKs or queue capacity.
Publication after fsync only updates bounded in-memory state and wakes subscribers.
CPU, disk bandwidth and the short publication lock are still shared process
resources; the service is not hard real-time isolated from subscriber workload.

## Observability

`firehose_stats` (no params) returns current count, archive tip, active subscriber
IDs, inclusive next heights, lag, mode, queued bytes/blocks and counters. Totals
survive subscriber removal. Subscriber detail is bounded by the active cap.

Prometheus `/metrics` exposes:

- `firehose_request_window_blocks`: available depth below tip capped by the request window.
  A fresh archive has depth zero even when bootstrapped at a high height. Scrapes locate
  the retained prefix boundary with logarithmic metadata probes, assuming the archive's
  consecutive-height contract. I/O probe failures report NaN. This observation never
  changes admission or cursors, and does not measure elapsed time or detect interior holes.
- `firehose_connections` counts WebSocket sessions, including peers without subscriptions.
- `firehose_subscribers` and `firehose_subscription_lag_blocks` (sum of active lag).
- `firehose_subscription_{demotions_total,blocks_sent_total,bytes_sent_total,catchup_seconds_total}`.
- `firehose_subscription_disconnects_total{reason}`.
- `firehose_subscriber_lag_blocks{subscriber}` and
  `firehose_subscriber_{demotions_total,blocks_sent_total,bytes_sent_total,catchup_seconds_total}{subscriber}`.

Sent counters measure successful bounded transport enqueue, not confirmed client
processing. Byte counters count raw record bytes. Catch-up seconds include disk,
socket and ACK time and update during active catch-up, not just on exit. Lag is
relative to the durable archive, distinct from the existing scheduler-tip lag.
Per-subscriber termination logs include ID, reason, cursor and final counters,
including accumulated catch-up time. Active metric labels are removed on exit;
historical per-subscriber analysis uses these logs. Disconnect reasons include
`client_closed`, `catchup_timeout`, `catchup_falling_behind`, `archive_timeout`,
`archive_error`, `archive_corrupt`, `archive_gap`, `record_too_large`, `encoding_error`, and
`height_exhausted`. Request rejections do not count as accepted subscribers.

Default jsonrpsee WebSocket pings reclaim unresponsive connections. Peers that
answer pings can remain connected without subscribing; the connection gauge exposes
their use of the shared connection cap.
