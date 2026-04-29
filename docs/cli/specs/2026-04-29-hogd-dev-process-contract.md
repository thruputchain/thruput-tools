# Hogd-Dev Process Contract

**Date:** 2026-04-29
**Status:** Draft contract proposal. PR 2 of the node-supervisor plan should not
implement against this until hogd owners agree to the binary name, readiness
mechanism, endpoint listing, and shutdown behavior.

**Companion to:**

- [`../plans/2026-04-29-local-node-hogd-dev-supervisor.md`](../plans/2026-04-29-local-node-hogd-dev-supervisor.md)
  - Rust node-supervisor implementation plan.
- [`../2026-04-25-hogd-dev-node-hybrid.md`](../2026-04-25-hogd-dev-node-hybrid.md)
  - big-picture hogd-owned hybrid dev-node design.

## Scope

`hogd-dev` is the single hogd-owned child process supervised by
`groundhog-node-supervisor`. It owns the local chain state, mempool, execution
path, read APIs, and dev-control APIs. Rust owns only process lifecycle, dev
account/genesis input, JSON-RPC facade wiring, log forwarding, and shutdown.

## Contract Checks Before PR 2

This document intentionally leaves some implementation choices as proposed
defaults. Before implementing the fake-child adapter in PR 2, agree on these
checks with hogd owners:

- binary name and invocation shape;
- readiness transport and JSON schema;
- endpoint names, address format, and whether read state is one endpoint or
  several named endpoints;
- shutdown grace timeout and when the supervisor escalates to `SIGKILL`;
- whether hogd-dev stderr is stable NDJSON and whether timestamps are produced
  by hogd-dev or added by the Rust supervisor;
- supervisor log-forwarding model: parse and re-emit through tracing, or
  pass-through with a stable prefix;
- v1 platform target, with Unix-only acceptable until SDK developer demand says
  otherwise;
- exact meaning of "sibling path next to `groundhog`" for bundled releases;
- typed startup errors for not found, configured path missing, and configured
  path not executable.

## Startup

The supervisor resolves the child binary in this order:

1. explicit `--hogd-dev-binary`;
2. `GROUNDHOG_HOGD_DEV_BINARY`;
3. sibling path next to the `groundhog` binary;
4. `PATH`;
5. typed startup error.

Proposed v1 invocation shape:

```bash
hogd-dev \
  --listen-host 127.0.0.1 \
  --scheduler-port 0 \
  --read-port 0 \
  --control-port 0 \
  --genesis-stdin \
  --readiness stdout
```

Rust writes the dev genesis payload to stdin. v1 is ephemeral; no state
directory is part of the contract until hogd-dev persistence is real.

## Readiness

Preferred v1 readiness is exactly one stdout JSON line. Stdout is otherwise
reserved for machine protocol; ordinary logs go to stderr.

```json
{
  "status": "ready",
  "network_id": "groundhog-local",
  "endpoints": {
    "scheduler": "127.0.0.1:12345",
    "read": "127.0.0.1:12346",
    "control": "127.0.0.1:12347"
  }
}
```

If startup fails before readiness, hogd-dev should exit non-zero and write a
clear error record to stderr. The supervisor treats child exit before readiness
as a typed startup failure.

## Logging

Stderr should use newline-delimited JSON records:

```json
{"level":"info","target":"hogd_dev","message":"scheduler listening","fields":{"addr":"127.0.0.1:12345"}}
```

The supervisor may forward these into its tracing/log output. Human formatting
belongs in the Rust CLI layer, not in the child process protocol.

## Shutdown

`SIGTERM` asks hogd-dev to stop accepting new requests, drain or fail in-flight
internal tasks deterministically, and exit within the documented grace timeout.
The supervisor sends `SIGKILL` to the process group only after that timeout.

On Linux, the supervisor may set `PR_SET_PDEATHSIG` for best-effort cleanup if
the parent dies. macOS has no direct equivalent; v1 documents that orphaned
children are a best-effort cleanup risk rather than adding a persistent agent.

## Required APIs

The ready endpoints must be sufficient for:

- `groundhog_chainTip`;
- `groundhog_pendingTransactionCount` as a real mempool count;
- `groundhog_sendRawTransaction`;
- transaction receipt lookup;
- transaction status, including pending-vs-committed semantics;
- block lookup once hogd-dev can answer it;
- contract event queries;
- dev-control methods needed for reset and transaction detail.

If pending and committed state live behind separate endpoints, the Rust adapter
owns the composite transaction-status orchestration described in the shared
RPC-server backend contract.
