# Hogd-Dev Process Contract

**Date:** 2026-04-29
**Status:** Current implementation contract for the `groundhog node`
supervisor path, with future extensions called out below.

**Companion to:**

- [`../../archive/cli/2026-04-25-hogd-dev-node-hybrid.md`](../../archive/cli/2026-04-25-hogd-dev-node-hybrid.md)
  - big-picture hogd-owned hybrid dev-node design.

## Scope

`hogd-dev` is the single hogd-owned child process supervised by
`groundhog-node-supervisor`. It owns the local chain state, mempool, execution
path, read APIs, and dev-control APIs. Rust owns only process lifecycle, dev
account/genesis input, JSON-RPC facade wiring, log forwarding, and shutdown.

## Current Contract

- **Process name.** The child binary is `hogd-dev`.
- **Readiness.** The supervisor starts `hogd-dev` with `--readiness stdout`
  and reads exactly one capped JSON readiness line from stdout. Current
  readiness is a pointer to a versioned runtime manifest. Stdout is otherwise
  reserved for machine protocol.
- **Endpoints.** Current `hogd-dev` writes canonical endpoint keys into the
  runtime manifest: `scheduler`, `indexer`, `dbGateway`, and `viewGateway`.
  The Rust supervisor still accepts older compact/long-form inline readiness
  aliases while parsing, but validation for the local-node path requires the
  runtime manifest pointer.
- **Dev accounts.** The supervisor passes deterministic dev-account count,
  seed, and initial token flags. `hogd-dev` advertises the bootstrapped account
  metadata in the runtime manifest, and the supervisor validates it for
  manifest-backed `dev-N` signing.
- **Genesis.** Current `hogd-dev` normally loads dev-account genesis before
  readiness. The supervisor keeps an empty-genesis compatibility probe after
  readiness for older or reduced children that report no genesis block.
- **Logging.** The supervisor sets `HOGD_LOG_FORMAT=json`, reads hogd-dev
  stderr as newline-delimited JSON, and re-emits child logs through Rust
  `tracing` with `hogd_timestamp_ms`, `hogd_target`, `hogd_file`, `hogd_line`,
  and `hogd_fields` metadata. Non-JSON stderr is treated as a warning.
- **Shutdown.** SIGTERM asks `hogd-dev` to stop accepting new requests and exit
  normally. The supervisor sends SIGKILL to the process group only after the
  configured grace timeout.

## Startup

The supervisor resolves the child binary, named `hogd-dev`, in this order:

1. explicit `--hogd-dev-binary`;
2. `GROUNDHOG_HOGD_DEV_BINARY`;
3. sibling path next to the `groundhog` binary, using
   `std::env::current_exe().parent()`;
4. `PATH`;
5. typed startup error.

Current invocation shape:

```bash
hogd-dev \
  --listen-host 127.0.0.1 \
  --scheduler-port 0 \
  --readiness stdout \
  --dev-accounts 10 \
  --dev-account-seed groundhog-dev-accounts \
  --dev-account-tokens 1000000000
```

The supervisor also injects `HOGD_LOG_FORMAT=json`. `hogd-dev` remains
ephemeral; no state directory is part of the contract until persistence is
real. There are no `--read-port`, `--control-port`, or `--genesis-stdin`
flags in the current binary.

## Readiness

Readiness is exactly one stdout JSON line. Stdout is otherwise reserved for
machine protocol; ordinary logs go to stderr. Current readiness is a bounded
pointer to the runtime manifest:

```json
{
  "schemaVersion": 1,
  "status": "ready",
  "manifestPath": "/tmp/hogd-dev/runtime-manifest.json",
  "manifestHash": "0x..."
}
```

The supervisor opens the manifest path, verifies `manifestHash`, parses the
versioned runtime manifest, and derives its internal readiness view from that
manifest. The manifest contains `networkId`, backend endpoints,
`bootstrapAccountDerivation`, and `bootstrapAccounts`.

If startup fails before readiness, hogd-dev should exit non-zero and write a
clear error record to stderr. The supervisor treats child exit before readiness
as a typed startup failure.

## Logging

Stderr should use newline-delimited JSON records:

```json
{"timestamp_ms":1770000000123,"level":"info","target":"hogd_dev","message":"scheduler listening","fields":{"addr":"127.0.0.1:12345"}}
```

The supervisor forwards these into its tracing/log output. The tracing target
is `hogd_dev`, and the child target is preserved as `hogd_target`. Common
source metadata is promoted to `hogd_file` and `hogd_line`; child emission time
is promoted to `hogd_timestamp_ms` when present; the original fields object is
preserved as `hogd_fields`. Human formatting belongs in the Rust CLI layer, not
in the child process protocol.

`groundhog node` keeps forwarded hogd-dev info logs quiet by default so the
golden path shows only supervisor lifecycle/status output. `groundhog node -v`
/ `--verbose` enables the normal debugging view (`hogd_dev=info`), including
when combined with `--json` because logs stay on stderr. `RUST_LOG`
remains the expert override for exact filters and debug/trace output.

### Structured Logging Follow-Up

The current hogd JSON logger is an envelope bridge, not a full structured
logging system. It preserves severity, target, message, source file, and source
line, but the operational details from existing call sites remain inside the
formatted `message` string:

```json
{
  "level": "info",
  "timestamp_ms": 1770000000123,
  "target": "mempool",
  "message": "commit summary: height=1, block_id=..., duration=0.022",
  "fields": {
    "file": "rpc_from_coord.cc",
    "line": 1833
  }
}
```

That is enough for `groundhog node` to route, filter, and display child logs
without changing every hogd subsystem. It is not enough for users or tools to
query fields like `height`, `block_id`, `tx_hash`, `contract`, or `duration`
without parsing free-form text.

Follow-up when real user workflows justify it:

- add a structured-field logging API in hogd without replacing the existing
  formatted-message API;
- migrate only high-value events first, such as commit summaries, transaction
  submission/rejection, view execution, storage reads, and startup endpoint
  reporting;
- keep the JSON envelope schema compatible so `node-supervisor` can forward
  both legacy formatted records and newer structured records;
- measure overhead before enabling any richer structured logging on hot paths
  outside the developer-node bridge.

## Shutdown

`SIGTERM` asks hogd-dev to stop accepting new requests, drain or fail in-flight
internal tasks deterministically, and exit within the configured grace timeout.
The current default grace is 5s. Because hogd-dev is ephemeral, shutdown grace
exists for log drain and clean SIGTERM handling, not state persistence. The
supervisor sends `SIGKILL` to the process group only after that timeout.

On Linux, the supervisor may set `PR_SET_PDEATHSIG` for best-effort cleanup if
the parent dies. macOS has no direct equivalent; orphaned children are a
best-effort cleanup risk rather than adding a persistent agent.

## Required APIs

The ready endpoints must be sufficient for:

- `groundhog_chainTip`;
- `groundhog_pendingTransactionCount` as a real mempool count;
- `groundhog_sendRawTransaction`;
- transaction receipt lookup;
- transaction status, including pending-vs-committed semantics;
- block lookup once hogd-dev can answer it;
- contract event queries;
- dev-control methods needed for transaction detail and future targeted
  state surgery.

Runtime whole-node reset is deliberately not a required ready-endpoint API.
For a clean local chain, the supported shape is restart or a supervisor-owned
startup flag such as `groundhog node --reset` that clears the dev state before
hogd-dev launches. In-process reset spreads across RAMDB, mempool, auth,
event, indexer, and bootstrap lifecycles and is lower leverage than targeted
dev controls like balance/storage/code patching.

The Rust adapter owns the composite transaction-status orchestration described
in the shared RPC-server backend contract: scheduler pending state first, then
committed read state. If hogd-dev later exposes a unified status RPC, the
adapter can collapse to that.
