# Check gateway health and metrics

The gateway serves health and Prometheus metrics on a separate private listener.
Keep that listener behind the deployment's access controls.

## Local smoke

From the repository root, with a compatible real runtime:

```sh
THRUPUT_DEVNODE_BINARY=/absolute/path/to/hog-devnode \
  KEEP_WORKDIR=1 scripts/smoke-gateway-observability.sh
```

The script starts a local node and gateway, checks RPC, health, metrics, and debug
logging, then stops them. `KEEP_WORKDIR=1` retains its logs and results. Read the
[script](../../scripts/smoke-gateway-observability.sh) for the assertions.

## Inspect a gateway

Use the private listener or an SSH-forwarded endpoint. Adjust these URLs to the
running gateway:

```sh
export GATEWAY_RPC_URL=http://127.0.0.1:9546
export GATEWAY_OBS_URL=http://127.0.0.1:9646

curl -sS -i "$GATEWAY_OBS_URL/livez"
curl -sS -i "$GATEWAY_OBS_URL/readyz"
curl -sS "$GATEWAY_OBS_URL/health" | jq .
curl -fsS "$GATEWAY_OBS_URL/metrics" > /tmp/hog-gateway.metrics

curl -fsS -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getVersionInfo","params":{}}' \
  "$GATEWAY_RPC_URL" | jq .
```

`/livez` checks that the service is alive. `/readyz` and `/health` use cached
backend probes and freshness checks; they return 503 during startup or when
required health conditions fail. Inspect the required backends and snapshot age.
Readiness does not exercise every public method or establish a safe load limit.

## Diagnose a problem

Compare RPC errors and latency with backend health, admission, waiting, and
execution metrics. Fast backend calls with long local waits suggest queueing.
Backend duration includes transport and client scheduling. On shared gateways,
counter deltas include other clients. Missing samples and counter resets do not
mean zero load; scrapes can miss short gauge peaks.

Use `RUST_LOG=info,hog_gateway=debug` for local handler diagnostics. Tracing can
change performance; keep logging settings consistent across benchmark runs and
capture detailed traces separately. See [benchmarks](bench-scenarios.md).

## Source

- [HTTP endpoints](../../crates/json-rpc/json-rpc-gateway/src/observability.rs)
- [Health probes and readiness](../../crates/json-rpc/json-rpc-gateway/src/health.rs)
- [Metrics and labels](../../crates/json-rpc/json-rpc-gateway/src/metrics.rs)
- [Listener and probe configuration](../../crates/json-rpc/json-rpc-gateway/src/config.rs)
