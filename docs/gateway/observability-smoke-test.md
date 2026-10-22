# Gateway health and observability

The gateway serves health and Prometheus metrics on a separate private listener.
This keeps operational data out of the public JSON-RPC API and allows readiness
to reflect backend availability without making every scrape perform backend RPCs.

## Local smoke

From the repository root:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  KEEP_WORKDIR=1 scripts/smoke-gateway-observability.sh
```

The script owns a local node and gateway, exercises RPC, health, metrics, and
bounded debug logging, and retains evidence when requested. Its assertions are
the executable smoke contract; do not maintain a second list of metric names
and expected version numbers here.

## Inspect a running gateway

Use the private listener or an SSH-forwarded endpoint:

```sh
export GATEWAY_RPC_URL=http://127.0.0.1:8545
export GATEWAY_OBS_URL=http://127.0.0.1:9646

curl -sS -i "$GATEWAY_OBS_URL/livez"
curl -sS -i "$GATEWAY_OBS_URL/readyz"
curl -sS "$GATEWAY_OBS_URL/health" | jq .
curl -fsS "$GATEWAY_OBS_URL/metrics" > /tmp/hog-gateway.metrics
```

`/livez` reports that the service is alive. `/readyz` and `/health` use cached
backend probes and freshness checks; they return 503 while starting or when
required health conditions fail. A live process can therefore be unready.
Inspect the configured required backends and the reported snapshot age rather
than treating any HTTP response as proof of readiness.

Compare the reported wire version with the actual target, not a version copied
into a runbook:

```sh
curl -fsS -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getVersionInfo","params":{}}' \
  "$GATEWAY_RPC_URL" | jq .
```

Readiness probes do not exercise every method or validate a production load
limit. Test the public operation relevant to the deployment separately.

## Interpret observations

Compare RPC outcomes and latency with backend availability, admission, waiting,
and execution metrics. Fast backend calls with long local waits point toward
queueing; backend timing still includes transport and client scheduling and is
not an isolated measurement of hogd execution.

Missing counters can mean a path has not run. Missing samples or counter resets
must not be interpreted as zero load. Gauge peaks between scrapes can be missed.
On a shared deployment, counter deltas include other clients' traffic.
Labels must remain bounded; transaction hashes, raw unknown method names, and
arbitrary error strings do not belong in metric labels.

Native submission and transaction-lookup handler spans are DEBUG-level diagnostics;
normal INFO logging avoids formatting a transaction hash for each request.
Use `RUST_LOG=info,hog_gateway=debug` to enable
these spans for bounded local request diagnostics. The OTLP filter exports gateway
spans only at INFO and above, so these handler spans and their `tx_hash` fields
are absent from OTLP even when local DEBUG logging is enabled. Backend warnings
carry transaction hashes explicitly. Metrics and the dedicated OTLP request/backend
spans remain available independently of this setting.
Tracing adds work and can change measured performance. Keep logging policy
consistent across performance comparisons, and keep diagnostic captures separate
from baseline runs. See [benchmarking](bench-scenarios.md).

## Source and deployment ownership

- [observability.rs](../../crates/json-rpc/json-rpc-gateway/src/observability.rs): HTTP endpoints.
- [health.rs](../../crates/json-rpc/json-rpc-gateway/src/health.rs): cached probes and readiness.
- [metrics.rs](../../crates/json-rpc/json-rpc-gateway/src/metrics.rs): metric names and labels.
- [config.rs](../../crates/json-rpc/json-rpc-gateway/src/config.rs): listener and probe controls.
- [Smoke script](../../scripts/smoke-gateway-observability.sh): repeatable verification.

Scrape targets, firewall rules, and dashboards belong to the deployment's ops
configuration. Bind observability to a reachable private interface only when
that deployment provides the corresponding access controls.
