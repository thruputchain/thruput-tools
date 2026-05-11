# `groundhog test --json` Schema and Ownership

This note applies the repo-wide schema ownership rules to
`groundhog test` before implementation begins.

The canonical repo-wide rules live in `CLAUDE.md` under
`Schema Ownership`. This document only decides how those rules apply to
the test command.

## Type Ownership

`groundhog test --json` is a CLI output contract, not a JSON-RPC
surface. Its top-level report and streaming event types therefore live
in `crates/cli`, not in `crates/api-types`.

Initial home:

- `TestReport`
- `TestSummary`
- `TestCase`
- `TestRun`
- watch-mode streaming records, if introduced

Suggested module names:

- `crates/cli/src/test_json.rs`
- `crates/cli/src/test_report.rs`

Either is fine; keep the types adjacent to `test_cmd.rs`.

## Reuse Rules

Do not copy RPC-facing nested types into the test schema.

If `groundhog test --json` needs to embed real chain or runtime data,
reuse the existing Rust types from `crates/api-types` instead of
creating lookalike structs in the CLI crate.

Examples:

- reuse `HexBytes`, `TxHash`, or `TransactionReceipt` if those exact
  concepts appear in test output
- keep purely test-runner concepts such as per-test timing, pass/fail
  status, retries, filters, and rerun metadata in CLI-local types

## Versioning

Stable `groundhog test --json` output must include a top-level
`schemaVersion` field inside the command's `data` payload from v1
onward.

Example envelope:

```json
{
  "ok": true,
  "command": "test",
  "warnings": [],
  "data": {
    "schemaVersion": 1
  }
}
```

The shared `CommandResult` envelope is not versioned per command. The
version belongs to the command-specific payload inside `data`.

## Watch Mode

If `groundhog test --json --watch` emits multiple results over time, it
should emit one complete JSON object per completed run. In practice this
means newline-delimited JSON records on stdout, each with the same
`CommandResult<TestReport>` outer shape and the same
`data.schemaVersion`. This includes both `ok: true` and `ok: false`
records; consumers should key off the envelope's `ok` field instead of
assuming success-only stdout.

## v1 Shape Requirements

The first structured `test` schema should prioritize typed fields over
opaque `cargo stdout` scraping. Raw stdout or stderr may still be kept
as optional debug fields, but they should not be the primary contract.

Required v1 expectations:

- per-run typed payload in `data`
- `data.schemaVersion = 1`
- structured per-test records
- structured run summary
- structured rerun metadata when `--rerun` is active
- stable watch-mode record shape if `--watch` is active

## Aggregate Summary Semantics

Per-run `summary.filteredOut` preserves the libtest-reported
invocation-local count.

For aggregated command-level summaries spanning multiple rerun
invocations, `summary.filteredOut` is set to `0`. Libtest reports that
value per invocation, and those counts do not compose meaningfully
across multiple exact-filter reruns.

## Extraction Rule

Do not create a separate shared CLI schema crate yet.

Extract one later only if at least one of these becomes true:

- multiple CLI commands reuse the same machine-readable types
- another crate in this workspace needs to depend on the test schema
- external tooling starts consuming the test schema as a broader public
  contract
