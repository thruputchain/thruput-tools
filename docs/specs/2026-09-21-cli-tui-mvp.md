# Groundhog Command UI System — MVP

- **Status:** Active implementation
- **Created:** 2026-09-21
- **Revised:** 2026-09-23
- **Tracking:** GH-388; GH-389–GH-396, GH-416, GH-417
- **Stack:** Rust, Ratatui, Crossterm; selected Rat-Widget/Rat-Focus components as needed
- **Scope:** All existing commands and their output variants, delivered in tested slices

## 1. Product decision

Keep the existing individual-command workflow. Give it one reusable, responsive
terminal UI system with consistent progress, result cards, warnings, and errors.

```console
groundhog status
groundhog build
groundhog airdrop
groundhog wallet create
groundhog test --watch
```

The shell remains the place to choose commands. Each invocation owns its UI only
for its existing lifetime. There is no full-screen launcher, persistent workspace,
new `tui` entry point, navigation application, or background UI service.

### Component foundation

Use rounded cards, muted labels, compact layouts, full values, and accurate
rendering at the current terminal width. Shared Ratatui widgets and a Crossterm-styled
append-only writer sit behind the `Print` facade for static cards, tables, outcomes,
findings, errors, prompts, and output-mode policy.

Status is the static reference command. Build is the live-output reference.
Migrate the remaining command families in small, tested chunks. Broad command
coverage is an acceptance requirement.

## 2. Common command presentation

The default human-terminal pattern is:

```text
[optional review / existing confirmation]
[spinner + current phase + elapsed time, while work is pending]
╭─ ✓ Outcome title ─────────────────────╮
│ Label:             Result             │
│ Label:             Full value         │
╰───────────────────────────────────────╯
⚠ Warning and existing hint, if any
Next: existing useful follow-up, if any
```

On failure, finish the progress region and display the shared error presentation:

```text
✗ Operation failed
  Specific error message
  Code: stable existing error code, where available
  Hint: existing actionable hint, where available
```

An error may include a result card for partial/completed work or diagnostic data.
Do not print a success card followed by a second contradictory top-level failure.
The command's established outcome determines its title and symbol.

### Consistency rules

- Most ordinary human commands end with one bordered result card. Read commands
  use descriptive titles such as `Groundhog Status` or `Wallets`, not a claim that
  the environment or contract execution succeeded merely because the query returned.
- Slow work uses the shared spinner/phase component. Delay its first display by
  about 150 ms so fast commands do not flash. Never add artificial work or delay
  completion to show an animation.
- Finalize progress before printing the final result, warnings, or errors. Success
  must not leave a loading indicator behind.
- Warnings and hints follow results in a stable order. Warnings about an outcome
  do not silently convert it to failure or success; existing exit rules still apply.
- Human diagnostics use a colored severity/icon heading with a muted code, followed
  by an indented message. Optional hints have a separate cyan `↳ Hint` heading and
  indented action text. Keep continuation lines aligned and the reading width no
  wider than the preceding result card. ASCII mode uses `> Hint`; plain output
  retains the existing `hint:` format.
- Empty results, no-op changes, dry-runs, declined actions, interrupted waits,
  partial success, and running services have explicit shared labels and symbols.
- A declined confirmation is `Cancelled` or the command's existing equivalent,
  not an error or completed action. Preserve the existing exit code.
- A next-step block is optional and reuses existing guidance. Do not invent recovery
  actions or add a new confirmation just to fit a visual pattern.

### Deliberate variants of the pattern

| Command shape | Presentation |
| --- | --- |
| Fast lookup or local mutation | Result card; spinner only if the real work lasts long enough. |
| Build/test | Live progress plus streamed diagnostics; final result/error with existing details. |
| Paginated/list output | Bordered results/table, count and cursor metadata; explicit empty state. |
| Existing wizard/confirmation | Shared review/question, then normal progress and outcome. |
| Watch loop | One result/error per iteration; shared waiting state between iterations. |
| Node/docs service | Startup progress, then a Ready card and logs; the command stays running. |
| Raw data producer | Preserve exact payload on its existing stream; shared prompts/diagnostics only. |

These are components of one system, not separate command-specific renderers.

## 3. Reusable component contract

Component names below describe responsibilities, not a required public Rust API.
Keep the existing `Print` facade where practical; migrate its implementation and
callers rather than create a competing output API in every module.

| Component | Owns |
| --- | --- |
| `ResultCard` | Rounded border, title/outcome marker, ordered label/value rows, optional rich content, width-aware wrapping and stacking. |
| `Progress` | Delayed spinner, phase label, elapsed time, optional real measured progress, completion/cancellation cleanup. |
| `Outcome` | Shared labels and symbols for success, informational/empty, no-op, dry-run, cancelled, partial, failed, ready/running, and unconfirmed outcomes. |
| `WarningList` | Ordered warnings, severity symbols, full messages, codes where already exposed, and existing hints below the card. |
| `ErrorMessage` | Stable error code/message/hint and optional structured context, with one consistent top-level failure format. |
| `ResultList` / `ResultTable` | Headers, rows, empty state, long-cell wrapping, narrow stacked records, and pagination metadata inside a result card. |
| `Prompt` / `Review` | Existing question/default/confirmation semantics and temporary focus/input ownership where needed. |
| `LogOutput` | Coordinated append-only diagnostics around the active progress region, without a custom log browser. |
| `NextSteps` | Consistent optional follow-up commands using the current safe command formatting. |

### Stack requirements

- Ratatui supplies the reusable human-TTY widgets, cell layout, and rendering.
  Crossterm supplies terminal sizing, supported styling/output, resize handling,
  and input/mode control when a prompt needs it.
- Static cards use the same widgets rendered once and committed to the normal
  screen. They do not need raw mode, a persistent event loop, or an alternate screen.
- Live components use a managed inline viewport and redraw only while the command
  is active. Do not continue using hand-assembled borders or one-row erase logic
  as a parallel implementation for normal TTY output.
- A shared plain-text renderer is required for pipes, JSON-adjacent diagnostics,
  unsupported terminals, and the explicit no-UI mode. It consumes the same result
  content and semantics rather than duplicating command business logic.
- Rat-Widget/Rat-Focus can supply prompt inputs/focus. Ratcn is optional for useful
  compatible components. Pick one focus/event owner; adopting every library is not
  a requirement. Do not create a component framework of our own.
- Use compatible released versions on the workspace's stable Rust toolchain.
  Verify dependency compatibility and keep raw-mode/input code out of display-only paths.

### Theme, color, and symbols

Define colors, spacing, borders, and symbols once. Neutral views, reviews, guided
setup, active states, and all card borders and titles use the terminal's cyan accent.
Standalone success, warning/partial, and error messages use green, amber, and red.
Query titles such as Balance stay neutral. Keep muted labels and the terminal background.
Outcome text is authoritative; color alone never establishes success.

| Meaning | Default treatment |
| --- | --- |
| Successful action | Green `✓` with an accurate outcome label. |
| Ready service / guided setup | Theme-accented heading and explicit ready/current state. |
| Working / waiting | Cyan spinner or waiting indicator and a phase label. |
| Warning / partial or unconfirmed result | Yellow `⚠` and explicit text. |
| Failure | Red `✗` and specific error text. |
| Information / empty / no-op / dry-run / cancelled | Neutral or cyan marker and explicit label; no misleading success claim. |

Use modest terminal-safe symbols with measured cell widths, not decorative emoji.
Provide an ASCII/plain fallback (`[ok]`, `[warn]`, `[error]`, `working...`) through
the shared capability policy. `NO_COLOR` removes color but retains readable labels
and symbols where supported. No outcome depends only on color or animation.

## 4. Responsiveness and terminal lifetime

Measure display cells, including wide characters, combining marks, and grapheme
clusters. Every component accounts for borders, padding, and labels before giving
space to values. Full result values must remain available; no ellipsis-only data loss.

| Available width | Required layout |
| --- | --- |
| 80+ columns | Compact aligned cards/tables within available width; default card maximum 100 columns. |
| 40–79 columns | Keep the border; wrap values and stack cramped fields or table records. |
| 24–39 columns | Compact bordered/stacked content where useful; reduce padding. |
| Too small for useful framing, or unknown capability | Shared unboxed plain presentation, with complete data and no minimum-size error. |

Prose wraps at words; long identifiers and paths wrap safely between graphemes.
Keep raw values distinguishable from continuation indentation. Prompts must retain
input and reachable controls on resize. Table columns must not hide required data;
use stacked records on narrow screens rather than silently dropping columns.

Static output may exceed terminal height and must scroll normally. The component
foundation must prove that tall cards/results can be committed without clipping
them to the current screen. Use supported rendering/commit behaviour and bounded
chunks where needed; do not implement a terminal emulator.

For live UI, react to resize and re-layout the current region. On short/narrow
terminals, reduce the progress region or switch safely to shared append-only output.
Ordinary commands and live progress must not clear shell output or guess how many
old wrapped rows to erase.

An approved confirmation may clear its own measured preview and question rows
before progress starts. Keep warnings for the result. Retain the preview after
resize/resume, when it exceeds the screen, or when input echo cannot be measured
safely. Declines and plain output remain visible. This does not clear the screen
or change confirmation rules.

Guided `init` is a deliberate exception: on a supported foreground terminal it
refreshes the visible display between input steps, showing all done/current/pending/
skipped states and only the current step's context. Use the shared stepper and
normal line prompts, not raw mode or a persistent application. Short terminals use
a compact heading or append-only fallback. Never purge scrollback. Keep final
results and warnings visible after the wizard finishes. Already-ready setup needs
only its result, without taking over the screen.

**After a command exits, its output is scrollback text.** Resizing then is handled
by the terminal emulator, and old borders may reflow. Neither Ratatui nor another
conventional renderer can redraw an exited command. Tests and documentation must
distinguish rendering at launch size, live resize, and post-exit scrollback reflow.
Do not keep short commands running just to preserve their borders.

## 5. Output modes and streams

All commands use one mode/capability policy. Individual handlers must not invent
their own TTY/color/prompt decisions.

| Mode | Required behaviour |
| --- | --- |
| Supported human terminal | Shared styled components; live progress when useful and compatible with the command. |
| `--json` | Existing JSON/JSONL shapes, versions, stream placement, warnings, and exit codes. No UI framing, animation, or ANSI escapes. |
| `--quiet` | Existing command-specific suppression/data/error behaviour. No spinner; do not hide data or warnings that currently remain visible. |
| Redirected/non-TTY or `TERM=dumb` | Shared plain append-only presentation for human diagnostics; preserve raw data channels. |
| `NO_COLOR` | Shared monochrome components; independent from whether cursor-based rendering is supported. |
| Global `--no-ui` | Shared plain append-only output with no boxes, animation, cursor control, or raw-mode widgets; current line prompts still follow their existing rules. |
| `--non-interactive` | Plain output and no prompts or child terminal input. Does not grant consent; keep explicit force/key-generation requirements. |

Define `--no-ui` and mode precedence in the foundation slice. JSON and quiet rules
take precedence; `--no-ui` never confirms an action. Keep `--offline` and
`--show-cargo-commands` semantics, including explicitly requested stderr traces.
No new config file or environment variable is required.

Preserve each command's stdout/stderr contract. Status/progress normally belongs
on stderr. Data currently printed on stdout stays there. A human stdout table may
be styled only when that stream is a terminal and the output contract permits it;
its piped/quiet/JSON representation retains existing semantics.

For initial live-mode eligibility, require supported terminal stdin/stdout/stderr
and no incompatible direct child I/O. Redirection selects shared append-only mode.
This conservative choice must not suppress data or reinterpret command results.

### Raw-output exceptions

- `keys export`: secret bytes remain exactly on the existing stdout/JSON channel.
  Never place secret material in cards, progress, logs, or retained UI state.
- `codegen ts` without `--output`: generated source remains raw stdout; with a file
  destination, use the common written-file result card.
- `completion`, `--skill`, and `--version`: preserve their script/document/version
  outputs. Do not wrap payloads in cards or add success chatter.
- `toolchain which`: preserve the directly usable path on stdout.
- Clap help/usage/parser errors retain their syntax, streams, and exit behaviour;
  align supported styling where practical without replacing the argument parser.

These paths still receive shared error/prompt handling where applicable and have
explicit tests. They are deliberate contracts, not gaps in migration coverage.

## 6. Operation, output, and lifecycle boundaries

Commands supply semantic content and progress; components own visual presentation.
Reuse `Print::summary_card`, `summary_report`, `success`, `warn`, `command_error`,
and prompt methods as adapters where suitable. The renderer remains internal to
`groundhog-cli`; existing public JSON types are not changed to match UI widgets.

Define a small command-scoped rendering/session boundary and internal progress/
outcome types only as needed. Do not introduce a cross-command scheduler, new RPC
protocol, global runtime rewrite, or parallel implementation of domain operations.

### One terminal writer

- While a live region is active, coordinate command status, child stdout/stderr,
  explicit command traces, and tracing logs through one owner.
- Route completed diagnostic lines into normal scrollback once; finalize the
  result below them. Preserve order within each stream without promising a total
  order between independent stdout/stderr producers.
- Drain both child streams concurrently. Handle partial lines, carriage-return
  progress, and long messages. Keep queues/buffers bounded; coalesce progress but
  never silently drop errors, result data, or terminal outcomes.
- Apply backpressure or a controlled append-only fallback if necessary. Preserve
  existing command-owned report/capture buffers and parsers.
- Sanitize untrusted controls. Support safe child styles through one adapter or
  request plain child output; never replay arbitrary cursor controls into the region.
- A mode requiring inherited terminal I/O uses a deliberate shared handoff/plain
  path. Suspend the live region before handing control to the child, then use the
  common final result/error. Do not leave an old ad hoc renderer as its fallback.

### Input and cleanup

Progress-only and static components do not need to consume keyboard input or
enable raw mode. Preserve normal Ctrl+C delivery to foreground commands/children.
If an input widget requires raw mode, scope it to that prompt and explicitly route
interrupts through the existing cancellation path. Pasted text never submits an
action by itself.

Pause/finalize progress before existing line prompts, then resume after the answer.
Preserve defaults, explicit `--force`, EOF, declines, quiet/non-TTY refusal, and
preflight ordering. No automatic retries, new confirmation steps, or authorization
bypasses are introduced by presentation changes.

Finish/restore the region on success, error, interruption, recoverable rendering
failure, and before panic diagnostics where possible. Test suspend/resume on
supported Unix terminals. A UI error must not replay side effects or invent success.
Child cleanup, installation leases, signal handling, and node shutdown remain
owned by the existing operations; make only the adaptations the renderer requires.

### Outcome correctness

- Distinguish query completion from the returned execution/health result. A status
  report with findings and a view with a reverted outcome must say so.
- Distinguish preparation, signing, admission, commitment, and successful execution.
  `--dry-run` is not `view`, and neither is a submitted transaction.
- Keep available hashes and facts when submission/receipt outcome is uncertain.
  An interrupted wait is “stopped waiting”, not “cancelled transaction”.
- Preserve multi-step partial results, including native deployment verification and
  deploy success with an alias-save warning. Do not advise resubmission as a UI repair.
- Keep `CliError` codes/hints/details and existing command-reported failure payloads.
  Central error handling must not duplicate errors already emitted by a command.
- Retain existing exit codes even when the human layout or wording is improved.

## 7. Complete command migration matrix

The command definitions and recursive `--help` audit contain **44 leaf commands**.
Every row has an owner and requires an explicit migration/compatibility check.
There is no “only if convenient” or “untouched legacy renderer” category.

Patterns: **Result** = shared result card/outcome, **Query** = shared data/list/table
result, **Service** = startup/ready/log lifecycle, **Raw** = preserved data payload
with shared surrounding diagnostics. Progress appears only when real work warrants it.

| Command | Pattern / important variants | Owner |
| --- | --- | --- |
| `status` | Query; complete/missing/invalid environment and profile, findings, outside project. | GH-389 |
| `build` | Result + logs; package/workspace resolution, stack size, target directory, bindings, Cargo passthrough. | GH-390 |
| `test` | Result + logs/loop; all test modes and flag families below. | GH-392 |
| `deploy` | Review + Result; package/artifact, alias, wallet, dry-run/live, partial alias-save failure. | GH-393 |
| `call` | Review + Result; named/raw entrypoint, typed/raw args, gas/wallet, dry-run/live, failed/uncertain execution. | GH-393 |
| `transfer` | Review + Result; amount/recipient/source resolution, dry-run/live, self-transfer and execution errors. | GH-393 |
| `airdrop` | Review + Result; local/hosted faucet, default/explicit recipient and amount, dry-run/live, warnings and attempt recovery. | GH-393 |
| `wallet create` | Review + Result; offline native identity, local binding, dry run, and explicit funding next step. | GH-393 |
| `view` | Query; metadata/raw calldata, caller override, decoded/raw return/abort/fault/events. | GH-391 |
| `balance` | Query; default/named/direct/dev target, zero balance versus lookup error. | GH-391 |
| `storage` | Query; raw address/key/value, absent value and RPC error. | GH-391 |
| `tx show` | Query; pending/committed/invalid/expired/unknown, missing receipt, raw/decoded data. | GH-391 |
| `tx list` | Query; target, limit/cursor, sparse/empty/exhausted page, root-invocation-only history. | GH-391 |
| `events list` | Query; topics 0/1/2, spec/event filters, cursor/limit, raw/decoded/malformed records. | GH-391 |
| `token transfers` | Query; from/to/account filters, cursor/limit, mint/burn, malformed data and base-unit amounts. | GH-391 |
| `config get` | Query; profile, merged fields and `--sources`, missing/invalid state. | GH-391 |
| `config profiles` | Query; active/missing/invalid/global/project profile listing and empty state. | GH-391 |
| `keys ls` | Query; public key/default markers and empty state; preserve stdout/quiet behaviour. | GH-391 |
| `wallet ls` | Query; saved bindings/defaults, missing ownership and empty state. | GH-391 |
| `toolchain list` | Query; catalog/installed/selected/stale state and existing stdout contract. | GH-391 |
| `toolchain which` | Raw path; missing/stale selection uses shared diagnostics. | GH-391 |
| `new` | Result; counter/stub templates, path/name validation and existing next steps. | GH-396 |
| `init` | Prompted steps + Result; new/partial/already-ready state, explicit key generation, non-interactive mode, versioned init JSON. | GH-396, GH-439 |
| `config new` | Result; global/project scope, fields, select default, atomic failure/no mutation. | GH-416 |
| `config set` | Result; typed fields, scope/profile, changed/no-op/invalid result. | GH-416 |
| `config unset` | Result; removed/no-op field and revealed inherited value. | GH-416 |
| `config use` | Result; saved selection and environment override warning. | GH-416 |
| `config rm` | Review + Result; scope, confirmation, removed contents/default selection. | GH-416 |
| `keys generate` | Result; optional/default name, duplicate/invalid failure; preserve existing hidden option handling. | GH-416 |
| `keys import` | Result; hidden/piped secret input, native StrKey/hex seed, default-selection warning, secret-safe errors. | GH-439 |
| `keys use` | Result; selected/nonexistent/invalid key, actual saved-default mutation. | GH-416 |
| `keys rm` | Review + Result; decline/force/non-TTY/missing key, default cleanup. | GH-416 |
| `keys export` | Raw secret; current authorization/loading order and output bytes; shared prompt/errors only. | GH-416 |
| `wallet use` | Result; selected/default wallet and owner validation. | GH-416 |
| `wallet rm` | Review + Result; scope/inheritance/default binding, missing/declined/removed. | GH-416 |
| `toolchain install` | Result + acquisition; exact/default version, cache hit/download, offline, lock/cancellation/failure. | GH-417 |
| `toolchain use` | Result; valid/stale/invalid selection. | GH-417 |
| `toolchain clean` | Result; default/exact/retired version, absent/no-op, active installation lock. | GH-417 |
| `chain dump` | Review + Result; stopped-node/data-dir checks, archive path, I/O failure. | GH-417 |
| `chain restore` | Review + Result; stopped-node/archive checks, destination, I/O failure. | GH-417 |
| `codegen ts` | Raw stdout or written-file Result; spec/load/generation failure, JSON source output. | GH-417 |
| `node` | Service; acquisition/startup/ready/failure/shutdown and all existing options below. | GH-395 |
| `docs` | Service; bundled/override snapshot, port selection/conflict, version warning, stop. | GH-395 |
| `completion` | Raw script/JSON; all supported shells, updated flags/help. | GH-394 |

### Flag and execution families that must be covered

Each owner records the presentation and tests for every distinct output branch.
Shared component tests cover visual combinations; command tests cover semantics.
Do not claim an exhaustive Cartesian product of every possible flag value.

- **Every command:** normal success, validation/preflight error, operational error,
  quiet, JSON where supported, redirected streams, `TERM=dumb`, no color, no UI,
  and current exit behaviour. Exercise warnings/empty/no-op/cancelled paths when
  they exist. Keep invalid option combinations rejected by the current parser.
- **Target resolution:** explicit and inherited profiles, RPC/runtime-manifest
  overrides, package/target directories, existing environment precedence, outside
  project context, global/project config scopes, and long/untrusted values.
- **Test:** package and `--workspace`; one-shot and `--watch`; `--integration`;
  match/skip/rerun/fail-fast; show-output/no-capture/thread count; harness passthrough;
  verbose/protocol/devnode-version options; setup/build/test/cleanup failures.
  Unsupported combinations stay rejected. Watch emits the existing result/JSONL
  per iteration. Workspace preserves package order and partial/skipped reports.
- **Node:** explicit/managed/environment binary selection, offline/cache/download,
  host/port, persistence/ephemeral state, account funding/count, artifact directories,
  protocol/readiness overrides, block driver/timing, RPC limits, RAM/genesis options,
  verbose/tracing, startup/runtime/shutdown errors. Preserve existing lifecycle.
- **Transactions:** confirmation before action, force, dry-run, validation failure,
  signed/prepared versus submitted, successful/failed execution, timeout/unknown,
  local/remote faucet paths, offline native wallet creation, and alias-save warnings.
- **Raw/framework output:** exported key bytes, generated source, completion scripts,
  toolchain paths, `--help`, `--version`, and `--skill` remain compatible and unframed.

A complex mode may intentionally use the new shared append-only renderer rather
than animate. Its final results, warnings, and errors still use shared components/
policy. “It still prints through the old path” is not migration completion.

## 8. Phases, owners, and review gates

| Issue | Slice | Depends on |
| --- | --- | --- |
| GH-389 | Shared Ratatui components, theme/symbols, output policy, reusable success/error/warnings/prompts, and status reference migration. | None |
| GH-390 | Inline runtime, output coordination, cleanup, and build reference migration. | GH-389 |
| GH-391 | Read/query/list/table commands and their data-output exceptions. | GH-390 |
| GH-392 | All test modes and output variants, in smaller PRs if needed. | GH-390 |
| GH-393 | Deploy/call/transfer/airdrop/wallet-create presentation and outcome paths. | GH-390 |
| GH-395 | Node and docs service lifecycle/log presentation. | GH-390 |
| GH-396 | New/init setup flows and integration of existing prompts. | GH-390 |
| GH-416 | Configuration/key/wallet management commands. | GH-390 |
| GH-417 | Toolchain actions, chain archives, codegen output. | GH-390 |
| GH-394 | Complete coverage audit, shared regression/terminal tests, guidance, release checks. | All other slices |

GH-389 defines prompt/error components; the migration issues wire them into their
own commands. GH-390 owns log routing and terminal lifetime; migration issues use
that implementation. Avoid duplicate ownership of a second renderer.

### Execution chunks

1. **Foundation:** preserve the confirmed status/card appearance using the actual
   new renderer. Demonstrate success, warning, error, empty, and narrow-layout
   components with deterministic fixtures. Verify static tall output and mode policy.
   Review before live work.
2. **Live reference:** migrate build, including slow work, warnings, failure, lots
   of output, resize, and interruption. Review the common lifecycle before rollout.
3. **Command families:** migrate one family or execution mode per chunk. Run its
   coverage checks and review representative output before the next chunk. Larger
   tickets can have several PRs; they are not instructions to rewrite everything at once.
4. **Coverage/release:** finish every matrix row and output branch, audit renderer
   bypasses, and verify supported terminal/platform behaviour.

Temporary legacy paths are acceptable only during migration and must stay tracked.
The workstream is complete when all commands have a shared presentation path or
an explicit tested raw/framework exception. Scope limits apply to new features
and architecture, not to leaving command families unreviewed.

## 9. Verification and completion criteria

### Component tests

- Render cards, tables, progress, outcomes, warnings, errors, prompts, and next
  steps at 120/80/60/40/24 columns and short heights. Check full data, borders,
  wrapping, empty rows, long codes/paths/URLs, Unicode, monochrome, and ASCII fallback.
- Use the same fixtures for TTY and plain modes to catch lost fields or conflicting
  outcome labels. Use focused snapshots and structural assertions.
- Check progress delay, terminal-state transitions, and one final outcome rather
  than duplicated spinner/success/error lines. No animation loop for idle static output.

### Process and compatibility tests

- Use pseudo-terminal tests for active resize, tall output/scrollback preservation,
  high-volume stdout/stderr, partial lines, input/paste/EOF, Ctrl+C, suspend/resume,
  and cleanup on failure/panic. Do not equate a correct current frame with preserved logs.
- Keep binary-level JSON/schema/stream/exit tests for every migrated family, including
  watch JSONL, partial test results, node/docs startup records, and secret/raw outputs.
- Test quiet mode against existing command-specific rules; status quiet findings
  must remain silent even though other quiet commands may still emit warnings.
- Exercise deliberately nonanimated modes and child terminal handoff. They must
  use the shared policy and remain compatible, not silently skip result/error migration.
- Audit direct `println!`/`eprintln!`, ANSI strings, bespoke borders/spinners, and
  command-local error formatting. Retain only documented raw/passthrough/framework
  boundaries. Command modules should pass content, not draw terminal chrome.

### Coverage record

GH-394 maintains a checked record per matrix row: mode mapping, covered execution
branches, regression tests, and any deliberate exception with its reason. Do not
close a migration ticket based solely on its happy-path screenshot.

### Release acceptance

- Ordinary commands consistently show useful progress followed by an appropriate
  bordered result/outcome, warnings below, or the shared error presentation.
- The new stack, not parallel hand-built renderers, owns normal human UI layout.
- All 44 commands and framework/raw outputs have verified disposition. New commands
  added during implementation must receive a row and owner before release.
- Full values, diagnostics, and existing machine/data contracts are preserved.
- No persistent TUI app or new domain behaviour has been introduced.
- Linux/macOS terminal and tmux smoke tests pass; no new OS support promise is implied.

Run relevant tests, formatting, and Clippy in each chunk. Before release run
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`, plus applicable local-node/contract smoke tests. Update
help/completions and canonical knowledge records as behaviour lands; regenerate/
check the bundle, run its tests, and verify package contents. Do not hand-edit
generated guidance or change JSON schema versions solely for human styling.

## 10. Sources and migration boundaries

- `crates/cli/src/main.rs`: current 44-command argument/dispatch surface.
- `print.rs`, `print/`, `terminal.rs`: shared facade, Ratatui components, capabilities,
  output formatting, and the live-task output migration boundary.
- `status.rs`, `checks.rs`: status presentation and authoritative findings.
- `lib.rs`, `build.rs`, `test_cmd.rs`: subprocess output, capture, and test parsing.
- `call.rs`, `deploy.rs`, `transfer.rs`, `airdrop.rs`, `wallet.rs`: transaction and
  multi-step outcome semantics; reuse rather than rewrite.
- `config_cmd.rs`, `keys.rs`, `toolchain_cmd.rs`, `chain.rs`, `codegen_cmd.rs`:
  management, raw data, and artifact paths.
- `node.rs`, `docs.rs`, `crates/node-supervisor/`: existing service lifetimes,
  signals, logging, publication, and cleanup.
- Existing `*_json.rs`, `result.rs`, `error.rs`, and CLI integration tests: machine
  contracts and failure behaviour.
- [Ratatui](https://ratatui.rs/), [inline viewport](https://ratatui.rs/examples/apps/inline/),
  [Crossterm](https://github.com/crossterm-rs/crossterm),
  [Rat-Widget/Rat-Focus](https://github.com/thscharler/rat-salsa),
  [Ratcn](https://github.com/kristoferlund/ratcn).
