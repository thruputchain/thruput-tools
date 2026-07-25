# Gateway Observability, Health, and Metrics Plan

## Goal

Add production-grade private health and Prometheus metrics endpoints to
`hog-gateway` for hosted/stable devnet monitoring, Grafana dashboards, and
future VictoriaMetrics compatibility.

The primary customer is the hosted monitoring stack. The endpoints and metrics
should help operators answer whether public JSON-RPC is healthy, whether the
gateway is keeping up, which backend plane is failing, and whether latency or
admission pressure is building.

## Non-Goals

- Do not expose observability endpoints publicly by default.
- Do not add JSON-RPC health methods.
- Do not route gateway metrics through Vector.
- Do not implement distributed trace propagation in v1.
- Do not put trace IDs, span IDs, transaction hashes, addresses, raw errors, or
  peer IPs into Prometheus labels.
- Do not add WebSocket subscription metrics until subscriptions exist.
- Do not require JSON log output for this work. A later `--log-format json`
  flag can improve log aggregation parity with hogd without blocking health or
  metrics.

## Decisions

- Add a private observability HTTP listener to `hog-gateway`.
- Expose `GET /livez`, `GET /readyz`, `GET /health`, and `GET /metrics` on that
  listener.
- Default the listener to a safe loopback address: `127.0.0.1:9646`.
- Use `9646`, not `9546`, because `9546` is already used throughout the repo as
  a standalone gateway JSON-RPC port.
- Prometheus scrapes the gateway directly with a new `gh-gateway` job.
- Hosted/stable readiness requires `scheduler`, `indexer`, `db_gateway`, and
  `view_gateway`.
- `/livez` is only process liveness. It must not fail because a downstream
  backend is down.
- `/readyz` is backend readiness. It may return `503` when required backend
  health fails.
- `/health` is a readiness-compatible alias for humans and uptime tools. It
  must not be wired as a liveness probe.

## Deployment Topologies

The gateway may be deployed either on the same instance as coordinator and the
hogd suite, or on a dedicated gateway instance.

Co-located gateway:

- Prometheus scrapes the same private instance IP for multiple jobs and ports.
- `gh-gateway` targets `<instance-private-ip>:9646`.
- Other jobs continue to target their existing ports, such as coordinator
  metrics and host metrics.

Dedicated gateway:

- Prometheus scrapes the dedicated private gateway IP on `9646`.
- The public or private JSON-RPC listener remains separate from observability.
- Security groups should allow the JSON-RPC listener according to the RPC access
  policy and allow `9646` only from monitoring, VPN, or operator CIDRs.

## Runtime Config

Add gateway config and CLI options:

```text
--observability-listen <addr>
--health-required-backends scheduler,indexer,db_gateway,view_gateway
--health-chain-tip-max-age-ms <ms>
--health-probe-interval-ms <ms>
--backend-connect-timeout-ms <ms>
--scheduler-reconnect-grace-ms <ms>
```

Suggested binary defaults:

```text
observability_listen = "127.0.0.1:9646"
health_required_backends = ["scheduler", "indexer"]
health_chain_tip_max_age_ms = 30000
health_probe_interval_ms = 5000
backend_connect_timeout_ms = 30000
scheduler_reconnect_grace_ms = 30000
```

Hosted release/ops override:

```text
--observability-listen 0.0.0.0:9646
--health-required-backends scheduler,indexer,db_gateway,view_gateway
```

The binary default stays permissive for local smoke/dev flows. Hosted/stable
configuration must make the full real RPC surface required.

## Health Endpoints

Endpoints:

```text
GET /livez
GET /readyz
GET /health
```

`/livez` behavior:

- Returns `200` as long as the observability HTTP server is responsive.
- Should return a small JSON body or plain `ok`.
- Intended for liveness probes only.
- Must not check scheduler, indexer, db gateway, view gateway, chain tip, or
  other downstream dependencies.

`/readyz` behavior:

- Reads cached probe state.
- Does not synchronously fan out to every backend per request.
- Background health task updates the snapshot on `health_probe_interval_ms`.
- Response body is JSON.
- Intended for readiness checks, load balancer target health, Prometheus
  blackbox checks, Uptime Kuma, and manual operator checks.

`/health` behavior:

- Alias `/readyz` for operator convenience and compatibility with common uptime
  tools.
- Must be documented as readiness, not liveness.
- Never wire `/health` to a liveness probe that can restart the process.

Readiness HTTP statuses:

- `200` for `ok`.
- `200` for `degraded`.
- `503` for `starting`.
- `503` for `error`.

Health status meanings:

- `starting`: first readiness probe has not completed.
- `ok`: all required backends are configured, reachable, and fresh.
- `degraded`: optional configured backend is failing, but required backends are
  healthy.
- `error`: any required backend is missing, unreachable, or stale.

The `degraded` state intentionally extends hogd's `starting|ok|error` model.
Shared alert rules and HTTP-status-only monitors should account for gateway-only
`degraded`; the `groundhog_gateway_health_status{status}` metric is the primary
machine-readable state.

Hosted/stable required backends:

```text
scheduler
indexer
db_gateway
view_gateway
```

Suggested `/readyz` and `/health` payload:

```json
{
  "service": "hog-gateway",
  "process_name": "hog-gateway",
  "pid": 1234,
  "executable_path": "/usr/bin/hog-gateway",
  "status": "ok",
  "version": "0.5.10",
  "commit_hash": "...",
  "uptime_seconds": 1234,
  "last_updated_timestamp": "2026-06-26T12:00:00Z",
  "snapshot_age_ms": 1200,
  "chain_tip": {
    "height": 42,
    "age_ms": 1200
  },
  "backends": {
    "scheduler": {
      "configured": true,
      "required": true,
      "up": true,
      "last_success_timestamp": "2026-06-26T12:00:00Z",
      "last_error": null
    },
    "indexer": {
      "configured": true,
      "required": true,
      "up": true,
      "last_success_timestamp": "2026-06-26T12:00:00Z",
      "last_error": null
    },
    "db_gateway": {
      "configured": true,
      "required": true,
      "up": true,
      "last_success_timestamp": "2026-06-26T12:00:00Z",
      "last_error": null
    },
    "view_gateway": {
      "configured": true,
      "required": true,
      "up": true,
      "last_success_timestamp": "2026-06-26T12:00:00Z",
      "last_error": null
    }
  }
}
```

The health payload intentionally uses `snake_case` to match hogd's plain HTTP
health shape. This endpoint is not a JSON-RPC method and is not bound by the
public JSON-RPC camelCase wire convention.

Required but missing backend example:

```json
{
  "configured": false,
  "required": true,
  "up": false,
  "last_success_timestamp": null,
  "last_error": "required backend is not configured"
}
```

`last_error` is operator-facing free text. Keep it bounded and sanitized. Do not
include raw backend payloads, user payloads, transaction bytes, addresses, hashes,
or other high-cardinality values.

`pid` and `executable_path` intentionally remain in v1 for hogd-style operator
diagnostics. They are acceptable only because the observability listener is
private/VPN-scoped; do not expose `/readyz` or `/health` on public RPC ingress.

## Metrics Endpoint

Endpoint:

```text
GET /metrics
```

Behavior:

- Returns Prometheus text exposition format.
- Uses a gateway-owned Prometheus registry.
- Compatible with Prometheus now and VictoriaMetrics later.
- Does not expose unbounded labels.

Metric prefix:

```text
groundhog_gateway_*
```

The gateway intentionally uses labeled Prometheus metrics rather than hogd's
current flat custom metric naming. Labels are bounded by the rules below.

The gateway intentionally uses histograms rather than summaries for latency.
Histograms can be aggregated across multiple gateway instances with
`histogram_quantile`; client-side summaries cannot.

Core availability metrics:

```text
groundhog_gateway_health_status{status}
groundhog_gateway_backend_up{backend}
groundhog_gateway_chain_tip_height
groundhog_gateway_chain_tip_age_seconds
groundhog_gateway_build_info{version,commit_hash,wire_format_version}
```

Do not add `groundhog_gateway_up`. Prometheus already generates `up{job="gh-gateway"}`
from scrape success. If the process is down, a target-exposed `*_up` metric cannot
emit `0`.

Prometheus metrics use base units, so tip freshness is exported in seconds as
`groundhog_gateway_chain_tip_age_seconds`. The health JSON keeps `age_ms`, and
the config keeps `health_chain_tip_max_age_ms`, because those are operator-facing
fields where millisecond precision is easier to read and configure.

`groundhog_gateway_build_info{wire_format_version}` uses the public JSON-RPC wire
version returned by `groundhog_version`, not the lower-level XDR protocol id. If
the XDR protocol id is useful later, expose it as a separate bounded label such
as `protocol_version`.

`groundhog_gateway_health_status{status}` uses enum-gauge semantics. Emit every
known state with `0` or `1`, with exactly one state set to `1`:

```text
groundhog_gateway_health_status{status="starting"} 0
groundhog_gateway_health_status{status="ok"} 1
groundhog_gateway_health_status{status="degraded"} 0
groundhog_gateway_health_status{status="error"} 0
```

Register standard process metrics where supported by the Rust Prometheus crate
and target OS:

```text
process_cpu_seconds_total
process_resident_memory_bytes
process_open_fds
process_max_fds
```

These process collectors require enabling the `prometheus` crate's `process`
feature. They are expected on Linux once that feature is enabled and may be
absent on macOS or other local development platforms. Tests must not assert
their presence unconditionally unless they are Linux-only and the feature is
enabled.

Tokio runtime saturation metrics are a follow-up if low-friction runtime metrics
are available.

RPC method metrics:

```text
groundhog_gateway_rpc_requests_total{method,outcome}
groundhog_gateway_rpc_request_duration_seconds_bucket{method,result,le}
groundhog_gateway_rpc_request_duration_seconds_sum{method,result}
groundhog_gateway_rpc_request_duration_seconds_count{method,result}
groundhog_gateway_rpc_in_flight{method}
```

Use the full bounded `outcome` set on counters. Use only `result="ok|error"` on
duration histograms to keep histogram series cardinality smaller and make p95/p99
queries stable.

Backend x23 metrics:

```text
groundhog_gateway_backend_requests_total{backend,operation,outcome}
groundhog_gateway_backend_request_duration_seconds_bucket{backend,operation,result,le}
groundhog_gateway_backend_request_duration_seconds_sum{backend,operation,result}
groundhog_gateway_backend_request_duration_seconds_count{backend,operation,result}
groundhog_gateway_backend_reconnects_total{backend,outcome}
```

Reconnect outcomes describe the connection attempt itself. Scheduler
`outcome="success"` means a replacement pool was built and entered quarantine;
handler readiness still requires the null-only grace period and a later tip probe.

Inbound JSON-RPC batch metrics:

```text
groundhog_gateway_rpc_batch_size_bucket{le}
groundhog_gateway_rpc_batch_size_sum
groundhog_gateway_rpc_batch_size_count
```

These measure client-supplied JSON-RPC request arrays. They are separate from the
internal submit/storage batchers below.

Oversized batch rejection happens before jsonrpsee `RpcServiceT::batch` in
jsonrpsee 0.25.x. Configure `BatchRequestConfig::Limit(1000)` on the server so
the rejection boundary exists. In jsonrpsee 0.25.x, an over-limit batch returns a
JSON-RPC error body with HTTP 200, so it is not visible to RPC middleware and is
not distinguishable by HTTP `status_class`. This is a known v1 observability
blind spot. A future pre-jsonrpsee tower layer can inspect array length, increment
an explicit rejection counter, and return a transport-level rejection.

HTTP transport metrics:

```text
groundhog_gateway_http_requests_total{endpoint,status_class}
groundhog_gateway_http_request_duration_seconds_bucket{endpoint,status_class,le}
groundhog_gateway_http_request_duration_seconds_sum{endpoint,status_class}
groundhog_gateway_http_request_duration_seconds_count{endpoint,status_class}
```

HTTP metrics should cover transport-level failures that never resolve to a
JSON-RPC method and surface as transport statuses, such as request timeout, CORS
rejection, body-size rejection, or malformed JSON that jsonrpsee maps to HTTP 400.
Some JSON-RPC pre-dispatch errors, including over-limit batches, can still return
HTTP 200 JSON-RPC error bodies and are therefore not reliably distinguishable by
HTTP `status_class` alone. The `endpoint` label must be a bounded route label such
as `jsonrpc`, `livez`, `readyz`, `health`, `metrics`, or `unknown`.

Connection metrics:

```text
groundhog_gateway_rpc_connections_active
groundhog_gateway_rpc_connections_limit
```

Expose `GatewayConfig.max_connections` as a gauge. Report active connection count
if jsonrpsee/tower wiring can observe accepts and closes without invasive
transport changes. `groundhog_gateway_rpc_connections_limit` is a hard DoD item;
`groundhog_gateway_rpc_connections_active` is best-effort for v1.

Batching/coalescing metrics:

```text
groundhog_gateway_internal_batch_size_bucket{component,le}
groundhog_gateway_internal_batch_size_sum{component}
groundhog_gateway_internal_batch_size_count{component}
groundhog_gateway_internal_batch_rejected_total{component,reason}
groundhog_gateway_internal_batch_in_flight{component}
groundhog_gateway_coalesced_waiters_bucket{component,le}
groundhog_gateway_coalesced_waiters_sum{component}
groundhog_gateway_coalesced_waiters_count{component}
```

Use `internal_batch` in the metric name so operators do not confuse internal
submit/storage batching with inbound JSON-RPC request batching.

Domain-specific metrics:

```text
groundhog_gateway_submitted_transactions_total{outcome}
groundhog_gateway_view_timeouts_total
groundhog_gateway_malformed_events_dropped_total{reason}
```

`groundhog_gateway_malformed_events_dropped_total` is for malformed event parsing
from backend event payloads, including indexer reads and ViewGateway events. It
is not a WebSocket subscription delivery-drop metric.
If subscriptions land later, add separate active-subscription and dropped-delivery
metrics.

Submit-to-commit latency is out of v1 scope because the gateway currently only
has scheduler-admission information in the submission path. Operators can compose
an initial SLO from `groundhog_gateway_submitted_transactions_total{outcome="ok"}`
and chain/indexer commit-rate panels. A future
`groundhog_gateway_submit_commit_latency_seconds` histogram should be added once
the gateway owns per-transaction inclusion tracking.

Allowed labels:

```text
method
backend
operation
outcome
result
component
status
reason
endpoint
status_class
```

Standard `reason` values:

```text
internal_batch_rejected_total.reason = closed | ingress_full
malformed_events_dropped_total.reason = parse_failed
```

Forbidden labels:

```text
tx_hash
address
raw_error
ip
cursor
topic
block_hash
trace_id
span_id
request_id
```

Method labels are bounded at the injection boundary. Map only known JSON-RPC
methods from `groundhog-json-rpc-api` to themselves. Include any jsonrpsee
built-in methods only if they are explicitly enabled by the gateway. Map all
unknown, malformed, or missing method names to `method="unknown"` or
`method="invalid"`. Never use an arbitrary client-supplied method string as a
label value.

Standard `outcome` values:

```text
ok
invalid_params
tx_invalid
tx_rejected
not_found
not_implemented
backend_unavailable
backend_timeout
overloaded
internal_error
```

The outcome mapping must be derived from the canonical JSON-RPC error catalog in
`crates/json-rpc/json-rpc-types/src/error.rs`, not hand-maintained in parallel.
Implement a tested mapping such as `outcome_for_code(code) -> &'static str` next
to the catalog, with a test that every `codes::ALL` entry maps to a bounded
outcome. The mapping may intentionally collapse `TxNotFound` and `BlockNotFound`
to `not_found`, and should include `ok` and `internal_error` for non-error and
unexpected paths.

Backend request metrics intentionally use the smaller transport/backend outcome
domain `ok | error | timeout | rejected` under the same `outcome` label name. RPC
method metrics use the JSON-RPC error-catalog-derived outcome set above.
Backend reconnect counters use `success | error | timeout`; the backend label is
the same bounded four-value domain as backend request metrics.

Standard `backend` values:

```text
scheduler
indexer
db_gateway
view_gateway
```

Standard backend `operation` values:

```text
schedcoord_get_tip
schedulercoord_null
schedcoord_submit_unvetted
schedcoord_lookup_transactions
indexer_block_explorer_intro
get_events
get_transaction_info
get_transactions_by_address
dbgateway_intro
dbgateway_get_items
viewgateway_intro
viewgateway_view
```

Standard `component` values:

```text
submit_batcher
storage_batcher
tip_coalescer
```

## Histogram Buckets

Pin bucket boundaries in code. Do not use library defaults implicitly.

RPC duration buckets, in seconds:

```text
0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60
```

Backend x23 duration buckets, in seconds:

```text
0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10
```

HTTP request duration buckets, in seconds:

```text
0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60
```

JSON-RPC request batch size buckets:

```text
1, 2, 5, 10, 25, 50, 100, 250, 500, 1000
```

Internal batch size buckets:

```text
1, 2, 5, 10, 25, 50, 100, 250, 500, 1000
```

Coalesced waiter buckets:

```text
1, 2, 5, 10, 25, 50, 100, 250, 500, 1000
```

These buckets can be tuned later, but changing bucket boundaries changes time
series shape and Grafana queries. Treat changes as dashboard-affecting.

## Health Probe Strategy

- Scheduler probe uses the existing chain-tip path as the main signal.
- Chain tip freshness is computed as time since the last successful scheduler tip
  probe. Under Groundhog's instant-finality model, tip age is a liveness/lag
  signal rather than a reorg-risk signal.
- Chain-tip freshness affects readiness only when `scheduler` is required. If the
  scheduler backend is deliberately optional, stale or unknown chain-tip data does
  not force `/readyz` to `503`.
- Cold-start chain-tip gauges emit `NaN` until the first successful tip probe.
  Alerts should key off `groundhog_gateway_health_status` for readiness and handle
  `NaN`/unknown tip values explicitly rather than treating unknown as fresh zero.
- `request_timeout_ms` is the end-to-end JSON-RPC deadline for both HTTP and
  WebSocket calls, including time spent in internal batch/coalescer queues.
  Backend RPC deadlines reserve a bounded completion margin inside that budget;
  the HTTP transport watchdog uses the same margin outside it. Expired queued
  waiters are discarded before backend dispatch. Internal batchers group
  waiters into earliest-deadline cohorts whose maximum skew is that completion
  margin, so a materially older waiter cannot share a backend timeout with
  newer work from the same drain. Scheduler-backed submit and tip paths process
  those cohorts newest-first because a timeout clears the active scheduler;
  storage reads remain earliest-first.
- The probe loop is non-overlapping, but probes the four active backends
  concurrently. An RPC against an existing client is capped by
  `min(request_timeout_ms, health_probe_interval_ms)`.
- Missing backends reconnect in one tracked background task per backend, capped
  by `backend_connect_timeout_ms`. Pool construction never delays health
  snapshot publication, and overlapping attempts for the same backend are not
  allowed.
- Initial and rebuilt scheduler pools are held outside the handler-visible
  client slot and checked only with the safe null RPC for
  `scheduler_reconnect_grace_ms`, so older hogd backends can finish coordinator
  registration after their TCP listener opens. The pool is promoted only after
  a successful null probe at or after the grace deadline; the next pass performs
  the first tip probe.
- Scheduler pools use rpc_driver's fail-closed cancellation policy. A timed-out
  scheduler request clears the handler-visible slot before canceling its worker;
  only the gateway reconnect manager may construct the replacement pool and put
  it through quarantine.
- Direct transaction-status scheduler lookups use reserved admission permits
  matched by scheduler pool capacity. Admission saturation returns overload
  without entering the x23 queue or clearing a healthy scheduler.
- Scheduler transport failures clear only the generation that produced the
  failure, so a stale request cannot remove a newer replacement pool.
- One active-client probe timeout marks that snapshot down but retains the pool.
  A second consecutive timeout discards it and starts the reconnect lifecycle.
  A successful response resets the timeout streak.
- The next loop sleeps for `health_probe_interval_ms` after the current pass
  completes.
- `/readyz` and `/health` also enforce a whole-snapshot freshness watchdog: if the
  cached snapshot is older than three probe intervals, readiness flips to
  `error`/HTTP 503 and the JSON exposes `snapshot_age_ms`.
- Indexer probe reuses `indexer_block_explorer_intro`, as already used at
  startup in `GatewayContext`.
- DbGateway probe reuses `dbgateway_intro`, as already used at startup in
  `GatewayContext`.
- ViewGateway probe reuses `viewgateway_intro`, as already used at startup in
  `GatewayContext`.
- Do not invent a second probe vocabulary where existing intro calls already
  prove cheap connectivity and protocol compatibility.
- Map health `degraded` by required-backend semantics: optional configured
  backend down is degraded; required backend down is error. Runtime probes keep
  rejection details in bounded `last_error` text and backend metrics, but startup
  may still abort on protocol/configuration rejection.
- Probe errors should be recorded in the health snapshot and backend metrics with
  bounded outcomes.

## RPC Instrumentation Approach

Preferred:

- Add gateway-local jsonrpsee RPC middleware with `set_rpc_middleware`.
- This lets method-level metrics include validation failures that happen before
  backend adapters and `groundhog_version`, without coupling the shared
  `json-rpc-server` crate to gateway metrics.
- Top-level parse/invalid-request failures that jsonrpsee rejects before RPC
  middleware classification do not have a trustworthy method label. Some surface
  as HTTP transport errors; others return HTTP 200 with a JSON-RPC error body and
  remain method-metric blind spots in v1.
- The middleware observes `MethodResponse::as_error_code()` and maps error codes
  through the canonical bounded `outcome_for_code(code)` helper.
- The middleware observes `Batch::len()` for inbound JSON-RPC batch size.
- Oversized JSON-RPC batches are rejected by jsonrpsee before RPC middleware when
  `BatchRequestConfig::Limit(1000)` is exceeded. jsonrpsee returns this as an
  HTTP 200 JSON-RPC error body in 0.25.x, so this rejection is a known v1 metrics
  blind spot.
- Bound method labels at the middleware boundary; unknown raw client method names
  map to `method="unknown"`.

Required HTTP-layer supplement:

- Add method-agnostic HTTP metrics around the JSON-RPC transport.
- This closes the blind spot for requests that fail before JSON-RPC dispatch and
  surface as transport statuses, such as CORS rejection, timeout, body-size
  rejection, or malformed JSON that jsonrpsee maps to HTTP 400.

Fallback:

- Instrument `GatewayBackend` first.
- This is faster but misses validation failures and all pre-dispatch transport
  failures.
- If fallback is chosen, track full RPC-layer and HTTP-layer instrumentation as
  follow-up issues.

## Implementation Plan

1. Add observability config to
   `crates/json-rpc/json-rpc-gateway/src/config.rs`.
2. Add CLI flags to `crates/json-rpc/json-rpc-gateway/src/main.rs`.
3. Add a `GatewayMetrics` module backed by `prometheus::Registry`.
4. Register process collectors where supported.
5. Add a `GatewayHealth` module with cached `HealthSnapshot`.
6. Spawn a background health probe loop after `GatewayContext` construction.
7. Spawn a small Axum server for `/livez`, `/readyz`, `/health`, and `/metrics`
   beside the jsonrpsee server.
8. Add graceful shutdown for the observability server alongside the RPC server.
9. Instrument HTTP transport count, duration, and status class.
10. Instrument RPC method count, duration, in-flight, and outcome.
11. Bound method labels to the known method set and `unknown`/`invalid`.
12. Instrument scheduler, indexer, db gateway, and view gateway backend calls.
13. Instrument inbound JSON-RPC batch size and set the jsonrpsee oversized-batch
    limit.
14. Instrument submit/storage batchers and tip coalescer.
15. Expose active connection count and connection limit if supported by the
    server wiring.
16. Update release env/systemd/docker defaults for the observability listener.
17. Update ops Prometheus config with a direct `gh-gateway` scrape.
18. Add Grafana panels for gateway health, latency, request rate, backend health,
    and overload.
19. Add tests for health JSON, metrics exposition, outcome mapping, bounded method
    labels, histogram buckets, and status transitions.

## Suggested Code Placement

- `crates/json-rpc/json-rpc-gateway/src/observability.rs`
- `crates/json-rpc/json-rpc-gateway/src/metrics.rs`
- `crates/json-rpc/json-rpc-gateway/src/health.rs`

Existing files likely touched:

- `crates/json-rpc/json-rpc-gateway/src/main.rs`
- `crates/json-rpc/json-rpc-gateway/src/config.rs`
- `crates/json-rpc/json-rpc-gateway/src/context.rs`
- `crates/json-rpc/json-rpc-gateway/src/backend.rs`
- `crates/json-rpc/json-rpc-gateway/src/submitter.rs`
- `crates/json-rpc/json-rpc-gateway/src/storage_batcher.rs`
- `crates/json-rpc/json-rpc-gateway/src/tip_coalescer.rs`
- `crates/json-rpc/json-rpc-gateway/src/handlers/events.rs`
- `crates/json-rpc/json-rpc-gateway/src/handlers/view.rs`
- `crates/json-rpc/json-rpc-types/src/error.rs`

## Prometheus and Ops Plan

Add port constant:

```text
GATEWAY_OBSERVABILITY = 9646
```

Static scrape job:

```yaml
- job_name: gh-gateway
  metrics_path: /metrics
  static_configs:
    - targets:
        - '<gateway-private-ip>:9646'
```

AWS EC2 discovery shape:

- Match gateway-capable instances by environment, deployment, and role tags.
- Support both dedicated gateway hosts and co-located validator/gateway hosts.
- Target `9646` by default.
- Optionally support a `gh:gateway-metrics-port` tag for non-default ports.
- Relabel `deployment`, `owner`, and environment consistently with existing
  jobs.

Security:

- Allow `8545` or the configured JSON-RPC port according to public/private RPC
  policy.
- Allow `9646` only from monitoring, VPN, or operator CIDRs.
- Do not expose `9646` to `0.0.0.0/0`.
- Never add `9646` to public JSON-RPC ingress rules. Public RPC ingress and
  private observability ingress must remain separate even when they are
  co-located on the same instance.

## Release Plan

Update `../release/`:

- Add the observability listener to `hog-gateway.env`.
- Add the observability listener to Docker/supervisor gateway commands.
- Optionally expose `9646` in dev compose.
- Add smoke checks:

```sh
curl -fsS http://127.0.0.1:9646/livez
curl -fsS http://127.0.0.1:9646/readyz
curl -fsS http://127.0.0.1:9646/metrics
```

Hosted gateway override:

```text
--observability-listen 0.0.0.0:9646
--health-required-backends scheduler,indexer,db_gateway,view_gateway
```

The flag name intentionally differs from hogd's `--metrics-port`: gateway accepts
a full listen address (`host:port`), while hogd accepts a port paired with its
bind IP. Operator docs should call out the mapping: hogd `--metrics-port` and
gateway `--observability-listen` both control private health/metrics HTTP.

## Grafana Initial Panels

- Gateway scrape `up{job="gh-gateway"}`.
- Gateway health status.
- Chain tip height.
- Chain tip age.
- RPC requests per second by method.
- RPC p95/p99 latency by method and `result`.
- RPC error rate by method/outcome.
- HTTP transport error rate by endpoint/status class.
- Backend up/down by backend.
- Backend p95 latency by backend/operation and `result`.
- Backend error rate by backend/operation/outcome.
- Active connections vs connection limit.
- Inbound JSON-RPC batch size.
- Submit/storage internal batch size.
- Tip coalesced waiter count.
- Overload/rejection rate.
- View timeout rate.
- Dropped malformed event rate.
- Process CPU, RSS, and file descriptors.

## Compatibility With hogd

This plan intentionally diverges from hogd in a few places:

- Gateway metrics use labels; hogd metrics currently use flat names. This is
  intentional because the Rust Prometheus crate makes bounded labels natural and
  dashboard queries clearer.
- Gateway latency metrics use histograms; hogd also has summaries. This is
  intentional because histograms aggregate across gateway instances.
- Gateway health has `degraded`; hogd has `starting`, `ok`, and `error` only.
  Alerting should use gateway-specific status rules.
- Gateway health JSON uses snake_case to match hogd's plain HTTP shape, even
  though JSON-RPC payloads use camelCase.
- Gateway exposes `/livez` and `/readyz`; hogd collapses liveness/readiness into
  `/health`. This is intentional to avoid orchestrator restart loops when a
  downstream backend is unhealthy.

Cheap parity wins to keep:

- Include `process_name`, `pid`, and `executable_path` in the health JSON.
- Use `/health` as a readiness alias for operator familiarity.
- Keep `/metrics` as the Prometheus endpoint name.

## Tracing Future-Proofing

V1 should keep metrics and span naming compatible with later distributed tracing.

Target span naming convention:

```text
gateway.rpc.groundhog_chainTip
gateway.rpc.groundhog_sendRawTransaction
gateway.rpc.groundhog_getTransactionReceipt
gateway.rpc.groundhog_getTransactionStatus
gateway.rpc.groundhog_getEvents
gateway.rpc.groundhog_getStorageAt
gateway.rpc.groundhog_getBalance
gateway.rpc.groundhog_view
gateway.backend.scheduler.schedcoord_get_tip
gateway.backend.scheduler.submit_unvetted
gateway.backend.scheduler.lookup_transactions
gateway.backend.indexer.get_transaction_info
gateway.backend.indexer.get_events
gateway.backend.db_gateway.get_items
gateway.backend.view_gateway.view
gateway.batcher.submit
gateway.batcher.storage
gateway.coalescer.chain_tip
```

This convention is target-state. Existing `#[tracing::instrument]` spans are
function-named today and can either remain in v1 or be renamed incrementally. The
important invariant is that future distributed tracing should not require metric
renames or label changes.

Metric labels must be sourced independently from span fields. Existing span
fields such as transaction hash are acceptable in spans/logs where sampled and
trusted, but they must never feed Prometheus labels.

Future propagation model:

- Accept trusted inbound HTTP `traceparent`.
- Generate a trace context when absent.
- Add optional XDR trace context for x23 calls.
- Gateway JSON-RPC request span becomes parent.
- x23 backend calls become child spans.
- Hogd services continue the trace as children.

Future XDR trace context fields:

```text
trace_id
span_id
parent_span_id
sampled
```

Important rule:

- Trace IDs and span IDs belong in spans/logs only.
- They must never be Prometheus labels.

## Testing Plan

Unit tests:

- Config parsing for observability fields.
- Required backend list parsing.
- Health status calculation.
- Health JSON serialization.
- Stale cached readiness snapshots flip to `error` and expose `snapshot_age_ms`.
- Outcome mapping to bounded labels.
- Test that every `codes::ALL` error code maps to a bounded outcome.
- Known-method mapping and unknown-method collapse.
- Metrics registry emits expected metric names.
- Histogram buckets match the pinned bucket lists.

Integration-style tests:

- Start observability server on `127.0.0.1:0`.
- `GET /livez` returns `200` without backend readiness.
- `GET /readyz` returns expected JSON and status.
- `GET /health` behaves as a readiness alias.
- `GET /metrics` returns Prometheus text.
- Health status transitions from `starting` to `ok`.
- Missing required backend returns `error` and HTTP `503` on `/readyz` and
  `/health`, while `/livez` remains `200`.
- Optional backend failure returns `degraded`.
- Stalled readiness probe loop returns `503` through `/readyz` and `/health` once
  `snapshot_age_ms` exceeds the watchdog threshold.

Gateway behavior tests:

- Backend timeout increments timeout outcome.
- Overloaded batcher increments rejection counter.
- Malformed event increments dropped-event counter.
- View timeout increments view timeout counter.
- Unknown JSON-RPC method does not create an unbounded method label.
- Transport-level malformed JSON or pre-dispatch rejection is counted by HTTP
  transport metrics when jsonrpsee surfaces it with a non-2xx status.
- Reason label domains are pinned by tests.

Manual smoke:

```sh
hog-gateway ... --observability-listen 127.0.0.1:9646
curl -fsS http://127.0.0.1:9646/livez
curl -fsS http://127.0.0.1:9646/readyz
curl -fsS http://127.0.0.1:9646/metrics | grep groundhog_gateway_build_info
```

## Definition of Done

- `hog-gateway` exposes private `/livez`, `/readyz`, `/health`, and `/metrics`.
- `/livez` is safe for liveness; `/readyz` and `/health` are readiness.
- Hosted config marks scheduler, indexer, db_gateway, and view_gateway as
  required.
- Prometheus can scrape the gateway directly as `gh-gateway` on port `9646`.
- Grafana can show scrape health, gateway readiness, RPC rate, latency, errors,
  backend health, connection limit, overload, and process resources where the
  required process collector feature/platform support is enabled.
- Metrics labels are bounded and cardinality-safe.
- Unknown method names collapse to a fixed label value.
- Duration histograms use pinned buckets.
- Health endpoint gives useful bounded failure reasons without requiring logs.
- Tests cover health status, metrics exposition, label/outcome mapping, bucket
  configuration, and unknown-method cardinality.
- Future trace propagation remains possible without metric or label renames.

## Follow-Up Scope

The following are intentionally deferred from v1 and should be tracked as follow-up
work rather than re-litigated in this PR:

- Pre-jsonrpsee oversized-batch rejection metrics. jsonrpsee currently returns an
  HTTP 200 JSON-RPC error for over-limit batches before RPC middleware sees them.
- `/readyz` drain-on-shutdown semantics and shutdown ordering with load balancers.
- Additional observability-server body-size and connection limits.
- Stronger `last_error` redaction beyond bounded, operator-facing text.
- Enabling Linux `process_*` collectors through the `prometheus` crate `process`
  feature.
- `groundhog_gateway_rpc_connections_active` if an accept/close hook can be added
  without invasive jsonrpsee transport changes.
- Submit-to-commit latency once the gateway owns per-transaction inclusion
  tracking.
