# Gateway Observability Smoke Test

Use this before opening the gateway observability PR against a running local or
private devnet gateway.

For a repeatable local smoke against a real `hog-devnode` backend, prefer:

```sh
GROUNDHOG_DEVNODE_BINARY=/path/to/hog-devnode \
  KEEP_WORKDIR=1 \
  scripts/smoke-gateway-observability.sh
```

The script starts `groundhog node` with interval block production, launches a
standalone `hog-gateway` from the runtime manifest, probes `/livez`, `/readyz`,
`/health`, `/metrics`, sends real `groundhog_version` and `groundhog_chainTip`
RPCs, and validates bounded debug trace lines in the gateway log.

## Preconditions

- `hog-gateway` is running with a private observability listener.
- Set the target addresses for the deployment under test:

```sh
export GATEWAY_RPC_URL="http://127.0.0.1:8545"
export GATEWAY_OBS_URL="http://127.0.0.1:9646"
```

For shared devnet or a remote private gateway, replace the host with the private
IP or SSH-forwarded local port.

## Local Tracing

For local debugging, run the gateway with gateway-level debug logs enabled:

```sh
RUST_LOG=info,hog_gateway=debug cargo run -p groundhog-json-rpc-gateway --bin hog-gateway -- ...
```

Useful debug/info events:

- Readiness status changes are logged at `info`.
- Steady-state readiness probe completions are logged at `debug`.
- JSON-RPC calls and batches log bounded method/outcome labels and duration.
- HTTP and observability endpoints log bounded endpoint/status labels and duration.
- Backend degraded-startup/probe failures and malformed events log warnings.

These tracing events are for local diagnosis; Prometheus remains the quantitative
source for counters, histograms, and dashboards.

## Health Endpoints

```sh
curl -fsS "$GATEWAY_OBS_URL/livez"
curl -fsS "$GATEWAY_OBS_URL/readyz" | jq .
curl -fsS "$GATEWAY_OBS_URL/health" | jq .
```

Expected:

- `/livez` returns HTTP 200 with `status=ok`.
- `/readyz` and `/health` return HTTP 200 when required backends are healthy.
- `/readyz` and `/health` JSON includes `snapshot_age_ms`, `chain_tip`, and the
  bounded backend keys `scheduler`, `indexer`, `db_gateway`, and `view_gateway`.
- If a required backend is intentionally stopped, `/readyz` and `/health` return
  HTTP 503 while `/livez` remains HTTP 200.

## Metrics Endpoint

```sh
curl -fsS "$GATEWAY_OBS_URL/metrics" > /tmp/hog-gateway.metrics
grep '^groundhog_gateway_build_info' /tmp/hog-gateway.metrics
grep '^groundhog_gateway_health_status' /tmp/hog-gateway.metrics
grep '^groundhog_gateway_backend_up' /tmp/hog-gateway.metrics
grep '^groundhog_gateway_rpc_connections_limit' /tmp/hog-gateway.metrics
```

Expected:

- `groundhog_gateway_build_info` includes `wire_format_version="2.1.0"`.
- Exactly one `groundhog_gateway_health_status{status=...}` series has value `1`.
- Backend labels are bounded to `scheduler`, `indexer`, `db_gateway`, and
  `view_gateway`.
- `groundhog_gateway_rpc_connections_limit` is present.
- Cold-start or missing chain-tip values may be `NaN`; readiness status is the
  authoritative health signal.

## JSON-RPC Method Metrics

```sh
curl -fsS \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"groundhog_version","params":[]}' \
  "$GATEWAY_RPC_URL" | jq .

curl -fsS "$GATEWAY_OBS_URL/metrics" > /tmp/hog-gateway.metrics
grep 'groundhog_gateway_rpc_requests_total{method="groundhog_version",outcome="ok"}' /tmp/hog-gateway.metrics
grep 'groundhog_gateway_rpc_request_duration_seconds_count{method="groundhog_version",result="ok"}' /tmp/hog-gateway.metrics
```

Expected:

- `groundhog_version` succeeds and returns `wireFormat`/`wire_format` matching
  `2.1.0` depending on the client view.
- RPC counter and duration histogram series are present with bounded method and
  outcome labels.

## Unknown Method Cardinality

```sh
curl -fsS \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"groundhog_nonexistent_smoke_method","params":[]}' \
  "$GATEWAY_RPC_URL" | jq .

curl -fsS "$GATEWAY_OBS_URL/metrics" > /tmp/hog-gateway.metrics
grep 'groundhog_gateway_rpc_requests_total{method="unknown",outcome="not_implemented"}' /tmp/hog-gateway.metrics
```

Expected:

- The raw unknown method name does not appear as a Prometheus label value.
- Unknown methods collapse to `method="unknown"`.

## Batch Behavior

```sh
curl -fsS \
  -H 'content-type: application/json' \
  -d '[{"jsonrpc":"2.0","id":3,"method":"groundhog_version","params":[]},{"jsonrpc":"2.0","id":4,"method":"groundhog_version","params":[]}]' \
  "$GATEWAY_RPC_URL" | jq .

curl -fsS "$GATEWAY_OBS_URL/metrics" > /tmp/hog-gateway.metrics
grep '^groundhog_gateway_rpc_batch_size_bucket' /tmp/hog-gateway.metrics
```

Expected:

- The successful two-call batch increments `groundhog_gateway_rpc_batch_size`.

Oversized batch known-v1 behavior:

```sh
python3 - <<'PY' >/tmp/hog-gateway-oversized-batch.json
import json
print(json.dumps([
    {"jsonrpc": "2.0", "id": i, "method": "groundhog_version", "params": []}
    for i in range(1001)
]))
PY

curl -sS \
  -o /tmp/hog-gateway-oversized-batch.response \
  -w 'http_status=%{http_code}\n' \
  -H 'content-type: application/json' \
  --data-binary @/tmp/hog-gateway-oversized-batch.json \
  "$GATEWAY_RPC_URL"

jq . /tmp/hog-gateway-oversized-batch.response
```

Expected:

- jsonrpsee rejects batches over 1000 before gateway RPC middleware sees them.
- Current jsonrpsee behavior is HTTP 200 with a JSON-RPC error body.
- There is intentionally no v1 `rpc_batch_rejected_total`; this blind spot is
  tracked in Linear `GH-155`.

## Domain Metrics

Exercise the relevant public methods if the deployment supports them:

```sh
curl -fsS "$GATEWAY_OBS_URL/metrics" > /tmp/hog-gateway.metrics
grep '^groundhog_gateway_internal_batch_size' /tmp/hog-gateway.metrics
grep '^groundhog_gateway_coalesced_waiters' /tmp/hog-gateway.metrics
grep '^groundhog_gateway_submitted_transactions_total' /tmp/hog-gateway.metrics || true
grep '^groundhog_gateway_view_timeouts_total' /tmp/hog-gateway.metrics || true
grep '^groundhog_gateway_malformed_events_dropped_total' /tmp/hog-gateway.metrics || true
```

Expected:

- Internal batch/coalescer metrics are present after `chainTip`, storage, or submit
  traffic.
- Submitted/view/malformed-event counters may be absent until those paths are
  exercised.
- All `component`, `reason`, and `outcome` labels remain in the documented bounded
  domains.

## Prometheus And Grafana

For shared devnet/nightly deployments with the ops changes applied:

```sh
curl -fsS "http://<prometheus-host>:9090/api/v1/targets?state=active" \
  | jq '.data.activeTargets[] | select(.labels.job == "gh-devnet-validator-gateway" or .labels.job == "gh-devnet-nightly-gateway") | {health: .health, instance: .labels.instance, lastError: .lastError}'
```

Expected:

- Gateway targets are `up` for instances running `hog-gateway` with
  `--observability-listen 0.0.0.0:9646`.
- The devnet validator dashboard gateway panels populate for health, RPC rate,
  RPC latency p95, and backend-up state.
