# Local Target Sweeper + Ownership Fields (Phase 2, Slice E) - Design

**Date:** 2026-05-27
**Status:** Implemented.
**Owner of implementation:** Codex.

This is a handoff design doc. It is self-contained: an implementer who has not
seen the planning conversation should be able to build from it.

## Summary

Add a `hoglab sweep` command that reclaims orphaned simulator-owned local
targets, and add the `target-ownership.json` process-identity fields the
sweeper needs to act safely. A sim-owned `groundhog node` (and the `hogd-dev`
child it supervises) is left running when the simulator process dies before its
normal cleanup runs. The sweeper finds those orphans, validates that a recorded
PID still belongs to our node, and terminates it.

This is a nice-to-have operational convenience, not a critical-path feature. It
is scoped deliberately small: one validation mechanism, one termination path,
and documented limitations instead of additional code paths for rare cases.

## Scope

This is **Slice E** of Phase 2. Slice A added the managed local provider, Slice
B added restart chaos, Slice D added baseline diffs. Slice E closes the
remaining tracker item "Add sweeper command for sim-owned local targets" and the
ownership-metadata enrichment it depends on.

### In scope

- Enrich `TargetOwnership` with an additive optional `process` block carrying
  the data a sweeper needs: pid, process-group id, port, process start time, and
  a command summary.
- The local provider populates `process` at acquire and refreshes it on each
  restart. The existing provider leaves it absent.
- A `sweep` module that scans a runs root, validates recorded PIDs, and
  terminates validated orphans by process group.
- A `hoglab sweep [root] [--dry-run] [--json]` subcommand.
- PID-recycle safety: validate a live PID's start time, process-group id, and
  command against the recorded values before signaling. Never terminate on a
  bare PID.
- Unit coverage for the data model, validation logic, and scan; one gated
  process-level smoke.

### Out of scope

- The "node already dead, `hogd-dev` orphaned on its own" case. The sweeper
  validates through the node PID; if the node is gone it cannot validate the
  surviving child and skips it. Documented as a limitation.
- Detecting or avoiding a concurrently active run. `hoglab sweep` is documented
  as a tool to run when no simulations are active. There is no heartbeat or lock
  file.
- Mutating or deleting swept run-directory artifacts. The sweeper terminates
  processes and reports; it does not touch files.
- `remote-ssh` sweeping (Phase 4).
- Windows support. The simulator's process control is already Unix-only.

## Architecture

### 1. Ownership data model

Add one optional field to `TargetOwnership` (in `crates/sim/src/target.rs`):

```
TargetOwnership {
  provider: TargetProviderKind,
  started_by_sim: bool,
  cleanup_allowed: bool,
  process: Option<OwnedProcess>,   // #[serde(default, skip_serializing_if = "Option::is_none")]
}

OwnedProcess {
  pid: u32,
  pgid: u32,
  port: u16,
  start_time: String,   // opaque token captured from `ps`, compared verbatim
  command: String,      // short argv summary, e.g. "groundhog node"
}
```

`OwnedProcess` is `#[serde(rename_all = "camelCase")]`, derives
`Debug, Clone, PartialEq, Eq, Serialize, Deserialize`, and carries `///` docs on
the type and each field. `TargetOwnership::existing()` leaves `process` `None`;
`TargetOwnership::local(..)` is given a populated `OwnedProcess`.

`start_time` is captured by running `ps -p <pid> -o lstart=` immediately after
spawn, trimming the trailing newline/column padding, and storing that opaque
token. It is never parsed into a timestamp; it is only ever compared against a
later `ps` reading normalized the same way. This avoids `/proc` parsing and
per-OS branching and works on both macOS and Linux.
`command` is the simulator's own short summary of the launch (e.g.
`"groundhog node"`), not a re-read of the live process; the live command is read
only at validation time.

`pgid` is the node's process-group id. The local provider already spawns the
node with `process_group(0)` (it is its own group leader) and `hogd-dev`
inherits that group, so terminating the group reclaims both.

If `ps` is not on `PATH` (checked with a direct Rust PATH search), or the
post-spawn `ps` read fails, the provider logs a warning and stores
`process: None`. The node still runs normally; only later sweep-ability is
lost. Capture failure never fails acquire. If process capture fails after a
restart, the refreshed ownership file clears `process` rather than leaving the
old PID behind.

### 2. Sweep module

New file `crates/sim/src/sweep.rs`. Pure logic, no CLI concerns.

Discovery:

- Input is a runs-root directory (default `hoglab-runs/`).
- Read each immediate child run directory's `target-ownership.json`.
- Immediate child directories without `target-ownership.json` are ignored, so
  arbitrary files or non-run directories under the root do not add report noise.
- An unreadable or unparseable ownership file is recorded as a skipped entry
  with a reason and does not abort the scan.
- Keep entries where `started_by_sim && cleanup_allowed && process.is_some()`.

Validation (the "never kill on a stale PID alone" rule):

- Run separate `ps` reads for `lstart=`, `pgid=`, and `command=` to avoid
  whitespace-sensitive parsing of fields that contain spaces.
- If `ps` reports no such process, the PID is dead: skip (nothing to reclaim).
- If the live `lstart` does not equal the recorded `start_time`: the PID was
  recycled to an unrelated process: skip.
- If the live process group does not equal the recorded `pgid`: skip.
- If the live command does not contain the recorded `command` marker: not our
  process: skip.
- Only an entry that passes all validation checks is eligible for termination.

Termination (PID-based, because the `Child` handle does not exist in a fresh
`hoglab sweep` process):

- `kill(-pgid, SIGTERM)`.
- Poll `kill(pid, 0)` until the process is gone, up to a bounded grace
  (reuse the local provider's existing grace constant).
- If still alive after the grace, `kill(-pgid, SIGKILL)`.
- Signaling uses `libc` directly, mirroring the existing local-provider
  teardown. SIGTERM-first lets the node reap `hogd-dev` cleanly; SIGKILL on the
  group is the fallback.

Output: a `SweepReport` listing, per run directory, one of
`reclaimed { pid, port }`, `skipped { reason }`, or `failed { error }`. The
report derives the serde shape used for `--json`.

### 3. `hoglab sweep` command

Add a `Sweep` subcommand (wired in `crates/sim/src/main.rs`, logic in a thin
`run` entrypoint that calls the `sweep` module):

```
hoglab sweep [root] [--dry-run] [--json]
```

- `root` defaults to `hoglab-runs/`.
- Default behavior terminates validated orphans and prints a human-readable
  summary of what was reclaimed, skipped, and failed.
- `--dry-run` performs discovery and validation and prints the candidate list
  and each validation result, but sends no signals. Reclaimable dry-run entries
  are reported as `state: "skipped"` with `reason: "dry run: would reclaim"`
  and include `pid`/`port`.
- `--json` emits the `SweepReport` as a single JSON document.

## Result / Output

`--json` emits one `SweepReport` document:

```json
{
  "schemaVersion": 1,
  "root": "hoglab-runs",
  "entries": [
    { "runDir": "hoglab-runs/1710000000-000000000-soak", "state": "reclaimed", "pid": 41234, "port": 9545 },
    { "runDir": "hoglab-runs/1709000000-000000000-smoke", "state": "skipped", "reason": "pid not alive" }
  ]
}
```

`state` is one of `reclaimed`, `skipped`, `failed`. `pid`/`port` are present for
`reclaimed` and dry-run reclaimable entries; `reason` for `skipped`; `error`
for `failed`. The report carries a `schemaVersion` from v1, matching the CLI
machine-readable-output convention. Report and entry fields are camelCase and
entries use this flat optional-field shape, not an internally tagged enum.

## Failure Semantics

- A single unreadable/unparseable ownership file, a dead PID, a recycled PID, or
  a command mismatch are all per-entry skips. The scan continues.
- `ps` missing on `PATH` at sweep time means validation cannot run: every
  candidate is skipped with that reason and nothing is signaled.
- A termination that fails after a target validated is a per-entry `failed`.
- The command exits non-zero only if at least one *validated* target failed to
  terminate. An all-skips run (nothing to reclaim) is success.

## Testing

Unit tests:

- `OwnedProcess` serde round-trip; `process` omitted from JSON when `None`;
  existing-target ownership has no `process`.
- Validation logic against captured/fake `ps` output: exact start-time match
  passes; mismatched start time skips; mismatched process group skips; missing
  process skips; command mismatch skips. Factor the `ps`-output comparison so
  it is testable without spawning a real process.
- Scan over a temp runs root containing a mix of: existing-target ownership
  (no `process`), a local entry with a dead PID, an unreadable file, and a
  well-formed local entry, asserting the expected per-entry classifications
  (with termination stubbed/avoided for the unit layer).
- `SweepReport` `--json` shape.

Process-level smoke (ignored, gated by `GROUNDHOG_SIM_HOGD_DEV_BINARY` and a
`groundhog` binary with dev-node support):

- Start a long local run through `hoglab run`, wait for
  `target-ownership.json` with process metadata, kill the `hoglab` parent
  process so the local target is orphaned, run `hoglab sweep`, and assert the
  entry is `reclaimed` and the RPC port is released afterward.

## Review Gates

- behavior/lifecycle reviewer: PID validation correctness (recycle safety),
  process-group termination, SIGTERM/SIGKILL ordering, `ps`-missing handling,
  capture-failure-does-not-fail-acquire.
- test/artifact reviewer: ownership additive-field backward compatibility,
  `SweepReport` JSON shape and `schemaVersion`, scan classification coverage,
  and documented-limitation accuracy.
