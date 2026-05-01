# Hogd-Dev Process Contract

**Date:** 2026-04-29
**Status:** Draft contract proposal. PR 2 implements a fake-child adapter
against these v1 choices; real hogd-dev integration still requires hogd-owner
agreement before depending on a real binary.

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

## v1 Reduced Contract

The full contract below describes the long-term shape. **The first hogd-dev
binary (gating PR 4a) implements a deliberately reduced subset.** When this
spec disagrees with the v1-reduced subset, the reduced subset wins for the
v1 binary; the long-form contract describes the target the binary grows
toward.

v1 reductions:

- **Endpoints.** Only `scheduler` (`SchedulerCoord1`) is bound and
  advertised in the readiness JSON. `read` and `control` are absent in v1
  and are *optional* in the readiness `endpoints` map. The supervisor
  treats absent keys as "stub plane stays NotImplemented in JSON-RPC,"
  not a startup failure.
- **CLI flags.** v1 accepts `--listen-host`, `--scheduler-port`, and
  `--readiness stdout`. **No `--read-port`, `--control-port`, or
  `--genesis-stdin` flags in v1.** They are added when the corresponding
  capabilities exist.
- **Genesis.** v1 hogd-dev does **not** load genesis itself. The
  supervisor loads genesis over `schedcoord_load_genesis_configuration`
  after readiness, lifted from chuckd-rs. See
  [`../plans/2026-04-30-hogd-dev-binary-bootstrap.md`](../plans/2026-04-30-hogd-dev-binary-bootstrap.md)
  for the rationale and provenance.
- **Logging.** v1 stderr is free-form text via existing
  `hogd::info/warn/error`. NDJSON formatter is a follow-up.
- **Shutdown.** v1 SIGTERM handler sets a `sig_atomic_t` flag; the main
  thread observes it via `sigprocmask` + `sigsuspend` (race-free) and
  returns normally from `main`, allowing RAII destructors to run —
  `HogdDevNode` performs `MempoolTestInfra`-style component teardown
  and `HogdDevTmpDir` removes the RAMDB temp directory. The destructor
  body is wrapped in `try/catch + CHECK(false)` so any throw becomes a
  loud abort. The supervisor contract treats clean teardown as
  best-effort: the supervisor's 5s SIGKILL escalation remains the
  safety net for destructor hangs (which the `try/catch` cannot
  catch). Net: the happy path cleans up after itself; the unhappy
  path is loudly killed within the grace window.

When v1 is replaced by a richer hogd-dev, this section can be deleted and
the long-form contract takes over verbatim.

## Contract Checks Before PR 2

This document intentionally leaves some implementation choices as proposed
defaults. Before replacing the fake child with real hogd-dev, confirm these
checks with hogd owners:

- binary name and invocation shape: recommended v1 binary is `hogd-dev`;
- readiness transport and JSON schema;
- endpoint names, address format, and whether read state is one endpoint or
  several named endpoints;
- shutdown grace timeout: v1 default is 5s, then the supervisor escalates to
  `SIGKILL`;
- whether hogd-dev stderr is stable NDJSON. Timestamps may be emitted by
  hogd-dev, but are not required by the v1 supervisor contract because tracing
  output already carries receipt-time metadata;
- supervisor log-forwarding model: v1 parses stderr NDJSON and re-emits through
  tracing, while tolerating non-JSON lines during bring-up;
- v1 platform target: Unix-only until SDK developer demand says otherwise;
- exact meaning of "sibling path next to `groundhog`" for bundled releases:
  v1 uses `std::env::current_exe().parent()`;
- typed startup errors for not found, configured path missing, and configured
  path not executable.

## Startup

The supervisor resolves the child binary, named `hogd-dev`, in this order:

1. explicit `--hogd-dev-binary`;
2. `GROUNDHOG_HOGD_DEV_BINARY`;
3. sibling path next to the `groundhog` binary, using
   `std::env::current_exe().parent()`;
4. `PATH`;
5. typed startup error.

Target invocation shape (long-form contract; see "v1 Reduced Contract"
above for what the first hogd-dev binary actually accepts):

```bash
hogd-dev \
  --listen-host 127.0.0.1 \
  --scheduler-port 0 \
  --read-port 0 \
  --control-port 0 \
  --genesis-stdin \
  --readiness stdout
```

In the target shape Rust writes the dev genesis payload to stdin. v1 is
ephemeral; no state directory is part of the contract until hogd-dev
persistence is real. **The v1 binary does not accept `--read-port`,
`--control-port`, or `--genesis-stdin`** — they are added when the
matching capabilities exist (see the v1-reduced section).

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

The supervisor forwards these into its tracing/log output. Human formatting
belongs in the Rust CLI layer, not in the child process protocol. Timestamp
fields are allowed but not required; tracing output provides receipt-time
metadata.

## Shutdown

`SIGTERM` asks hogd-dev to stop accepting new requests, drain or fail in-flight
internal tasks deterministically, and exit within the configured grace timeout.
The v1 default grace is 5s. Because v1 hogd-dev is ephemeral, shutdown grace
exists for log drain and clean SIGTERM handling, not state persistence. The
supervisor sends `SIGKILL` to the process group only after that timeout.

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

For v1, the Rust adapter owns the composite transaction-status orchestration
described in the shared RPC-server backend contract: scheduler pending state
first, then committed read state. If hogd-dev later exposes a unified status
RPC, the adapter can collapse to that.
