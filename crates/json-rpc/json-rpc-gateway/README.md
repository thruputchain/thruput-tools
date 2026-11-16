# hog-gateway

The standalone JSON-RPC gateway connects public clients to hogd's internal x23
services. It shares the public server implementation with the local node;
production connections, admission, deadlines, and recovery remain gateway-owned.
Read the
[architecture guide](../../../docs/architecture.md#from-application-to-runtime)
for those boundaries and the [JSON-RPC reference](../open-rpc-spec/snapshots/thruput-spec.md)
for methods and payloads.

## Build

From the workspace root:

```sh
cargo build --locked -p thruput-json-rpc-gateway --bin hog-gateway
cargo test --locked -p thruput-json-rpc-gateway
```

## Run

For local use, start a backend and obtain its runtime manifest using the
[runtime-manifest quickstart](../../../docs/gateway/runtime-manifest.md). Then:

```sh
hog-gateway --runtime-manifest "$RUNTIME_MANIFEST" --listen 127.0.0.1:9546
```

Use `hog-gateway --help` and
[GatewayConfig](src/config.rs) for options and defaults. `--dump-effective-config`
resolves configuration without opening listeners. Explicit backend flags can
override manifest endpoints; conflicting TOML backend fields are rejected.

Standalone configuration requires immutable `network_hash` identity;
runtime-manifest mode derives it from the manifest. Hosted faucet support is
optional. When enabled, the gateway validates the configured faucet's network
identity before forwarding airdrops, keeping signing material and faucet policy
in the faucet service. See [airdrop.rs](src/airdrop.rs) for startup and degraded
behavior.

### Client admission

The gateway enforces budgets per JSON-RPC call, including each array entry and
WebSocket message. Configure them independently of backend capacity:

```toml
[client_admission]
max_in_flight_calls = 4096
max_in_flight_per_connection = 64
client_rate_per_sec = 50
client_burst = 200
max_tracked_clients = 100000
trusted_proxies = ["127.0.0.1/32", "::1/128"]
```

Only trusted proxy peers may supply `X-Forwarded-For`; direct clients are keyed
by their peer address. IPv4 uses the full address and IPv6 uses its /64.
Concurrency rejection returns `-32053`; a client that exceeds its token budget
receives `-32055` (Rate limited) with `data.kind = "rateLimited"` and a decimal
string `data.retryAfterMs` delay in milliseconds. These
requests never reach a backend. Rejected notifications are dropped silently.
Discovery methods cost no tokens but still consume concurrency permits.
`simulateInvocations` conservatively costs `10 × view_batch.max_calls` per request.
See [client admission configuration](src/config/client_admission.rs) for validated
ranges and [admission](src/admission.rs) for the method cost table. Unknown clients
share an overflow bucket when the tracking cap is reached; idle entries are
evicted on insertion into their shard, sweeping at most once per second per shard.

Prometheus exposes `groundhog_gateway_admission_rejected_total` with the bounded
`reason` labels `global`, `connection`, and `rate_limited`, plus
`groundhog_gateway_admission_in_flight`, `groundhog_gateway_tracked_clients`, and
`groundhog_gateway_connections_open`. There are no per-client labels.

## Operate and measure

Heap profiling is opt-in on the existing private observability listener:

- Set `GROUNDHOG_HEAP_PROFILING=1` to activate profiling. The path returns 404
  when the switch is unset. `--dump-effective-config` bypasses profiling initialization.
- On a non-loopback `--observability-listen` address, the heap route stays 404
  unless `--pprof-allow-remote` or `GROUNDHOG_PPROF_ALLOW_REMOTE=1` is set.
  Metrics and health are unaffected; startup logs why the heap route is disabled.
- The heap route answers only GET/HEAD, and only when the `Host` header is
  `localhost` or a loopback IP (with the remote opt-in, any IP literal). DNS-name
  Hosts and cross-origin `Origin` headers get 403 to block DNS rebinding, so remote
  scrapes must address the listener by IP. `/metrics` and the health routes do not
  check `Host`, so Prometheus can still scrape by hostname.
- `GET /debug/pprof/heap` returns an uncompressed pprof protobuf with
  `Content-Type: application/octet-stream`.
- Dump and conversion run on a blocking worker; a concurrent dump request returns HTTP 429.
- Dumps require a writable temporary directory. Private dump files use mode 0600
  and are removed after each request.
- Build on Linux with `--features heap-profiling` to use jemalloc profiling with
  a 512 KiB average allocation sampling interval. Linux release archives,
  including the Zen 5 build, are built this way, with profiling inactive until
  `GROUNDHOG_HEAP_PROFILING=1`. Default builds keep the system allocator and
  fail startup if `GROUNDHOG_HEAP_PROFILING=1`.
- Profiles are symbolized in process with function names, plus file and line when
  debuginfo is present. Mappings carry GNU build IDs.

- [Health and metrics](../../../docs/gateway/observability-smoke-test.md)
- [Local benchmark scenarios](../../../docs/gateway/bench-scenarios.md)
- [Status-miss benchmark](../../../docs/gateway/lookup-bench.md)
- [Workspace documentation](../../../docs/README.md)
