# hog-gateway

The standalone JSON-RPC gateway connects public clients to hogd's internal x23
services. It shares the public server implementation with the local node;
production connections, admission, deadlines, and recovery remain gateway-owned.
Read the [gateway design](../../../docs/gateway/system-design.md) for those
boundaries and the [OpenRPC snapshot](../open-rpc-spec/snapshots/groundhog-spec.json)
for methods and payloads.

## Build

From the workspace root, with private Thruput registry access:

```sh
cargo build --locked -p groundhog-json-rpc-gateway --bin hog-gateway
cargo test --locked -p groundhog-json-rpc-gateway
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

- [Health and metrics](../../../docs/gateway/observability-smoke-test.md)
- [Local benchmark scenarios](../../../docs/gateway/bench-scenarios.md)
- [Status-miss benchmark](../../../docs/gateway/lookup-bench.md)
- [Workspace documentation](../../../docs/README.md)
