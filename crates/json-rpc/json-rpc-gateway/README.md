# hog-gateway

The standalone JSON-RPC gateway connects public clients to hogd's internal x23
services. It shares the public server implementation with the local node;
production connections, admission, deadlines, and recovery remain gateway-owned.
Read the [gateway design](../../../docs/gateway/system-design.md) for those
boundaries and the [OpenRPC snapshot](../open-rpc-spec/snapshots/thruput-spec.json)
for methods and payloads.

## Build

From the workspace root, with private Thruput registry access:

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

## Operate and measure

Heap profiling is opt-in on the existing private observability listener:

- Set `GROUNDHOG_HEAP_PROFILING=1` to activate profiling. The path returns 404
  when the switch is unset. `--dump-effective-config` bypasses profiling initialization.
- `GET /debug/pprof/heap` returns an uncompressed pprof protobuf with
  `Content-Type: application/octet-stream`.
- Dump and conversion run on a blocking worker; a concurrent dump request returns HTTP 429.
- Dumps require a writable temporary directory. Private dump files use mode 0600
  and are removed after each request.
- The switch is supported only on Linux; requesting it elsewhere fails at startup.
  Linux builds use jemalloc even with profiling inactive, and compile in profiling
  support with a 512 KiB average allocation sampling interval.

- [Health and metrics](../../../docs/gateway/observability-smoke-test.md)
- [Local benchmark scenarios](../../../docs/gateway/bench-scenarios.md)
- [Status-miss benchmark](../../../docs/gateway/lookup-bench.md)
- [Workspace documentation](../../../docs/README.md)
