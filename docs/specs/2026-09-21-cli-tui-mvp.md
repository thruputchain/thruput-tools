# Groundhog CLI TUI MVP

- **Status:** Draft for review; implementation has not started
- **Date:** 2026-09-21
- **Baseline:** Groundhog CLI 0.18.0; current command definitions and implementation
- **Stack:** Rust, Ratatui, Crossterm; selected Rat-Widget/Rat-Focus components
- **Entry point:** `groundhog tui`

## 1. Decision and product goal

Add a persistent terminal workspace for the existing contract development loop:

`check environment -> start local node -> build -> test -> deploy -> call/view -> inspect`

The user should keep project, profile, wallet, operation output, and results in one
place. The interface must remain usable in a small terminal and during long-running
work. It must reuse the existing command semantics, validation, codecs, and node
supervisor.

V1 targets a developer with an existing Groundhog project and configured profile.
Existing setup commands remain the way to prepare that environment. A missing
prerequisite produces a useful finding and the relevant command, not a new setup
workflow inside the TUI.

The TUI is an additional presentation layer. Direct commands, their exit codes,
confirmation rules, and JSON contracts remain supported. Launching `groundhog`
without a subcommand does not change in this release.

### Success criteria

A developer can select a package, run its build and RV32 tests, deploy it, execute
a contract call or view, and inspect the outcome without leaving the workspace.
They can start and stop one local node, or use an endpoint already configured in
a profile. Resizing, output volume, and errors do not corrupt the terminal or
require restarting the TUI.

### Explicit scope limits

- One invocation directory, one selected package, and one selected profile.
- One user-requested operation at a time, alongside at most one TUI-owned node.
- Four views: Overview, Build & Test, Contracts, and Activity.
- Manual refresh and rerun. No background chain dashboard or filesystem watcher.
- Simple text fields and selectors. No recursive form generator or embedded editor.
- Session-only UI state. No database, daemon, detach/reattach, or persistent job history.
- No new RPC methods, signing model, compiler pipeline, or protocol types.
- No generic command runner, shell panel, plugin system, or arbitrary CLI flag editor.
- No requirement to create a TUI screen for every existing CLI command.

## 2. Audit of the current CLI surface

The audit covered all 43 leaf commands through recursive `--help` inspection and
the argument/dispatch definitions in `crates/tools/cli/src/main.rs`. The table
records the complete surface and its v1 disposition.

**Included** means a native TUI action or view. **Read-only** means existing data
is displayed without exposing the command's mutation workflow. **CLI** means the
command remains available in the shell and has no v1 TUI implementation.

| Existing command | V1 | TUI mapping or reason |
| --- | --- | --- |
| `new` | CLI | Project creation and counter/stub template selection precede this workspace. |
| `docs` | CLI | Keep the existing local documentation server; no second server lifecycle in the TUI. |
| `init` | CLI | Keep tool/key/project setup in the existing wizard. |
| `status` | Included | Overview uses the shared findings/report collector. |
| `build` | Included | Build the selected package; optional TypeScript bindings. |
| `test` | Included | One-shot RV32 tests for the selected package; name filter and rerun failed tests. |
| `deploy` | Included | Selected package, optional alias, wallet, signed dry-run, and submission. |
| `call` | Included | Alias/address, method/entrypoint, arguments, wallet, gas limit, dry-run, and submission. |
| `view` | Included | Existing sandboxed simulation, including optional simulated caller. |
| `balance` | Included | One-shot balance for a supplied target or effective wallet. |
| `transfer` | CLI | Asset movement is outside the contract-loop MVP. |
| `airdrop` | CLI | Funding retains the existing local/remote faucet workflows. |
| `storage` | CLI | Raw address/key storage browser is deferred. |
| `node` | Included | Start/stop one local node with a small option set; view its output. |
| `completion` | CLI | Shell integration remains a shell operation. |
| `config get` | Read-only | Show persisted profile data and its sources in Overview details. |
| `config profiles` | Read-only | Populate the session profile selector. |
| `config new` | CLI | No profile creation form. |
| `config rm` | CLI | No profile deletion flow. |
| `config set` | CLI | No configuration editor. |
| `config unset` | CLI | No configuration editor. |
| `config use` | CLI | TUI profile selection is session-local and does not save a default. |
| `toolchain install` | CLI | Explicit version management is deferred; normal node startup acquisition still works. |
| `toolchain list` | CLI | Overview reports selected toolchain status, not a version catalog browser. |
| `toolchain use` | CLI | No change to the global toolchain selection. |
| `toolchain clean` | CLI | No cache-management workflow. |
| `chain dump` | CLI | Stopped-node archive operations are deferred. |
| `chain restore` | CLI | Stopped-node archive operations are deferred. |
| `keys generate` | CLI | Key creation remains part of setup. |
| `keys ls` | Read-only | Overview can show public key names and effective key information from existing reports. |
| `keys export` | CLI | Private key export is not part of a TUI result/log view. |
| `keys use` | CLI | Use existing effective signing-key resolution. |
| `keys rm` | CLI | Key deletion is deferred. |
| `wallet ls` | Read-only | Populate transaction-wallet selectors from existing bindings. |
| `wallet use` | CLI | TUI wallet choice is an operation override, not a saved default. |
| `wallet create` | CLI | Prefunding, faucet policy, and wallet creation remain in the existing workflow. |
| `wallet rm` | CLI | Wallet-binding removal is deferred. |
| `codegen ts` | CLI | Standalone spec-to-file generation is deferred; `build --bindings ts` is included. |
| `tx show` | Included | Look up a hash or inspect a session transaction using the existing public lookup. |
| `tx list` | CLI | Address history and cursor navigation are deferred. |
| `events list` | CLI | Event history/filtering is deferred; events already returned by an operation may be displayed. |
| `token transfers` | CLI | Token history and participant filters are deferred. |

### Existing options and v1 treatment

The following is a scope boundary, not a replacement CLI argument reference.
Arguments not surfaced in the TUI continue to work through their current commands.

| Area | Exposed in v1 | Existing options kept in the CLI |
| --- | --- | --- |
| Build | Package selection, TypeScript bindings toggle, session target directory | Stack-size override and Cargo passthrough arguments |
| Test | Package selection, `--match-test`, `--rerun` | `--workspace`, `--watch`, `--integration`, fail-fast, skip lists, output/capture controls, thread count, integration-node overrides, harness passthrough |
| Deploy | Selected package, alias, wallet, dry-run | Explicit artifact-path deployment |
| Call | Contract target, method or raw entrypoint, argument fields, wallet, gas limit, dry-run | No extra options beyond session target/profile handling |
| View | Contract target, method or raw entrypoint, argument fields/raw calldata, simulated caller | No extra options beyond session target/profile handling |
| Balance | Optional target | No extra options beyond session target/profile handling |
| Node | Port and optional persistent data directory; loopback host | Host exposure, account count/funding, explicit binary/version, artifact directories, protocol/startup overrides, block-driver timing, connection/batch limits, RAM database size, genesis overrides |
| RPC-backed actions | Selected profile and its existing environment/config resolution | Per-action RPC URL and runtime-manifest override fields |
| Transactions | Existing review/confirmation through TUI controls | No global confirmation bypass toggle |

Honor existing environment variables and configured defaults. For example, a
node binary supplied through `GROUNDHOG_DEVNODE_BINARY` still works. Options not
exposed in a form must not be silently given new values by the TUI.

### Semantics the presentation must preserve

- `view` is sandboxed simulation. Call/deploy dry-run prepares and signs without
  submitting; it is not simulation and can require a key and RPC access.
- Submission, commitment, and successful contract execution are distinct outcomes.
  A receipt timeout or interrupted wait does not prove transaction failure.
- Deploy can succeed on-chain while saving the local alias fails. Show the address
  and the alias warning together; do not offer redeploy as the repair action.
- `status` findings are useful content even when the CLI would exit with code 1.
  A failed test report must likewise remain visible rather than become a generic error.
- Persisted `config get` data excludes runtime overlays and command overrides.
  Show it separately from the effective execution context.
- Package names and contract aliases are distinct. A package supplies metadata;
  an alias resolves an address. An address has no automatic package association.
- The deferred `tx list` command reports root-invocation history, not a complete
  asset ledger. Deferred event/token commands retain their current cursor,
  filtering, raw-data, and amount semantics.

## 3. Launch and session context

Proposed syntax:

```console
groundhog tui
groundhog tui --profile local --package counter
groundhog tui --target-dir /path/to/build-output --offline
```

Add only `--profile`, `--package`, and `--target-dir` to the new subcommand.
`--offline` and `--show-cargo-commands` remain inherited global options.

- Require interactive stdin and the terminal output handle before entering raw
  mode. Use stderr for the TUI display, consistent with current human status output.
- Reject `--quiet`, `--json`, and `TERM=dumb` before terminal setup. A `--json`
  invocation receives the usual structured preflight error on stderr, with a
  nonzero exit status; it does not enter raw mode or produce a TUI result schema.
- `NO_COLOR` disables color, not interaction. Use labels and focus markers so the
  interface works without color.
- `--show-cargo-commands` routes invocation traces to the operation log, not the
  physical terminal. `--offline` keeps its existing managed-download meaning;
  it does not imply all RPC or Cargo activity is offline.
- `--help`, `--version`, and `--skill` retain their existing behaviour and exclusions.

Keep the invocation directory fixed for the lifetime of the TUI. Discover projects
with the existing helpers. Do not change the process working directory to switch
packages. In a workspace, require an explicit package selection if current CLI
resolution would be ambiguous.

Initial profile selection uses existing precedence, including explicit
`--profile` and `GROUNDHOG_PROFILE`. A subsequent picker selection becomes an
explicit override for that session. The UI shows the selected profile, effective
RPC URL, wallet, and key source. It does not call `config use`, `wallet use`, or
`keys use` just because the user selects an item.

An operation captures its inputs and resolved context when it starts. Disable
profile/package/wallet changes while an operation or transaction review is active.
While a TUI-owned node runs, keep the profile fixed; stop it before changing
profile. Historical results retain their original endpoint and context.

Refresh re-reads config and status. If an external edit removes a selection, mark
it unavailable and require a new choice. Do not substitute another transaction
target. Invalidate cached method metadata after a build or package change.

Outside a project, Overview and configured read-only actions remain usable.
Build/test/package deploy show a prerequisite message. Starting the UI itself
must not create a project, key, wallet, node, or download.

## 4. Views and interaction

Use a compact header for context, a view selector, the active view, and a footer
for available keys and the current operation. The output panel is part of the
active operation view, not another independent application.

### Overview

- Display existing environment, toolchain, key, and project findings.
- Provide profile and package selectors, effective context details, and Refresh.
- Provide a balance target field and a one-shot Read Balance action. Keep RPC
  errors distinct from a zero balance; no continuous balance polling.
- Display local-node ownership/state, port, persistence mode, and the owned-node
  log. Provide Start and Stop for the node owned by this session.
- Missing setup includes the relevant existing command, such as `groundhog init`
  or `groundhog wallet create --help`. The user runs it in the shell and refreshes.
- A configured endpoint is labelled as configured until a request verifies it.
  The existing status collector is not a new chain-health service.

### Build & Test

- Select a package, toggle TypeScript bindings, and run Build.
- Run RV32 tests once, optionally with a name substring; Rerun Failed uses the
  existing failure cache and target-selection semantics.
- Show operation phase, elapsed time, and live subprocess output. An activity
  indicator is sufficient; do not invent progress percentages.
- On completion show artifact paths/sizes or the existing test summary and case
  results. Preserve failing output and errors in the same view.
- Tests keep their current build prerequisites. Deploy keeps its existing
  artifact-resolution/build behaviour. The UI adds no new automatic pipeline.
- No live test-case parser is required: stream output during execution and show
  the structured report when the existing parser produces it.

### Contracts

- List aliases for the selected profile, with a direct-address field as an alternative.
- Keep the selected package visible as the source of method metadata. Suggest
  methods when existing local/built metadata is available; reuse the current
  resolver for the authoritative selection and validation.
- V1 uses one text field per argument when metadata supplies names/types. Complex
  arguments use the CLI's current textual/JSON encoding within that field. There
  is no nested struct/union/vector form builder.
- Without enumerable metadata, allow a method name with an ordered list of text
  argument fields, or an explicit entrypoint with one raw calldata field. Do not
  add a second shell-argument parser or fetch/infer an ABI from an address.
- Provide distinct View, Signed Dry-run, and Submit Call actions. Call exposes
  gas limit and wallet; View exposes its existing optional simulated caller.
- Deploy uses the selected package, wallet, and optional alias. Preserve existing
  alias defaulting and validation, including whitespace handling.
- Review live call/deploy with the resolved profile, endpoint, wallet, contract
  or artifact, and arguments/alias. The explicit Submit/Deploy control replaces
  the existing line confirmation. Escape returns to the form without submission.
- Reuse prepare/confirm/sign/submit boundaries. A review is tied to its prepared
  inputs. If preparation expires or needs to change, prepare again and show the
  updated review before submission. Do not implement confirmation by blindly
  invoking the old command handler with `force = true`.
- Show decoded return/abort data when the current command can produce it, and raw
  data when it cannot. Do not promise more receipt detail than the backend supplies.

### Activity

- Keep a bounded, session-only list of completed operations and submitted transaction
  hashes. This is session activity, not on-chain address history.
- Show result summaries, context, warnings, error code/hint, and full values in a
  scrollable detail view.
- Allow a transaction hash to be entered manually and looked up with `tx show`
  semantics. Refresh performs another one-shot lookup against that result's endpoint.
- Retain a hash as soon as it is available, including ambiguous submission errors.
  A stopped wait reads “Stopped waiting; outcome not confirmed”, not “Cancelled
  transaction”. Never retry a submission as a side effect of Refresh.
- Operation-returned events may appear in result details. There is no event
  subscription, event-history browser, or transaction trace explorer.
- Closing a result retains its summary in session history. Full output is retained
  only for the latest result per workflow; label older entries when that output
  is no longer available. Retained transaction hashes can still be looked up.

### Keyboard and terminal behaviour

| Input | Behaviour |
| --- | --- |
| `Tab` / `Shift+Tab` | Move focus within the active view or modal |
| Arrow keys | Navigate lists/selectors; edit text when a text field has focus |
| `Enter` | Activate the focused control; never submit a transaction merely by leaving a field |
| `Esc` | Close details/review or return to the parent; does not cancel background work |
| `F1` | Show contextual key help |
| `Ctrl+C` | Request cancellation of the current operation; when idle, request exit |
| `q` | Request exit when no text field is focused |
| Page Up / Page Down | Scroll the focused output/detail region |
| End | Return a log to its newest output |

View switching uses the focusable view selector. Global character shortcuts must
not intercept typed or pasted argument text. Pasting text never submits a form.
Mouse support is optional for v1; every action must work from the keyboard.

Use full-value detail views and terminal text selection for v1. A cross-platform
clipboard service, export flow, command palette, and user-configurable keymaps
are deferred. Do not rely on hover tooltips to expose essential information.

## 5. Responsive layout requirements

Use terminal-cell width and height, not byte/character counts. Recompute layout
on resize and clamp scroll positions while retaining focus and entered values.

| Available size | Layout |
| --- | --- |
| At least 110 columns and 28 rows | Main content and result detail may sit side by side. |
| At least 70 columns and 18 rows | One main pane; details replace the list or open a bounded modal. |
| At least 40 columns and 12 rows | One pane, stacked label/value fields, compact view selector and footer. |
| Below 40 columns or 12 rows | Minimal “Terminal too small” view with operation state and exit/cancel keys. Work and input state are preserved. |

Both dimensions matter. A wide but short terminal must use the compact layout.
Forms and result details scroll within the available region; they never require
an off-screen button. Avoid a fixed sidebar in v1.

Shorten addresses and paths only in summaries. Full values are accessible in
details. Wrap explanatory prose; allow horizontal scrolling for raw data/logs
where wrapping would obscure structure. Borders and padding give way to content
on narrow screens. Focus and errors remain distinguishable in monochrome.

## 6. Application and refactoring boundaries

Implement the TUI as modules in the existing `groundhog-cli` library, with a thin
dispatch entry in `main.rs`. Do not create a new public SDK, transport protocol,
or service crate for this release.

```text
Existing CLI arguments                 TUI events and forms
          |                                    |
          +------ shared command operations ---+
                            |
        config / build / test / client / node-supervisor

Text and JSON rendering                Ratatui rendering
```

### UI organization

Start with `tui/mod.rs`, `app.rs`, `event.rs`, `terminal.rs`, and one module per
view. Add a small operation adapter module if needed. Keep rendering separate
from state updates and I/O. Avoid a generic component framework of our own.

Application state holds context, the active view/modal, form values, one active
operation, node state, and bounded results/output. A typed event enum carries
input, resize, progress, operation completion, and node lifecycle changes.
Use operation identifiers to ignore late events from cancelled/replaced work.

### Reuse and extraction map

| Existing source | Required reuse or small extraction |
| --- | --- |
| `checks.rs`, `status.rs` | Use `collect_report`; render findings in both frontends. |
| `config/{project,read}.rs`, `config_cmd.rs` | Use existing profile precedence, runtime overlay, wallet/address resolution, and persisted-source reports. |
| `build.rs` | Reuse `build_rv32_project` and `BuildResult`; separate output delivery from execution. |
| `test_cmd.rs`, `test_json.rs` | Expose the existing one-package outcome/report before text/JSON rendering; retain report parsing and failure-cache writes. |
| `call.rs`, `deploy.rs` | Expose existing preparation, signing, submission, receipt interpretation, and alias-save steps behind frontend-neutral operations. |
| `view.rs`, `balance.rs`, `tx.rs` | Return their current lookup/simulation results before rendering. |
| `contract_spec.rs`, `codec`, `groundhog-client` | Reuse metadata resolution, encoding, decoding, and async invocation operations. |
| `node.rs`, `node-supervisor` | Reuse node options, acquisition, lifecycle, runtime publication, and cleanup. |
| `print.rs`, `lib.rs`, subprocess helpers | Add controlled progress/output delivery for included operations; preserve text/JSON behaviour. |

Not all boundaries already exist as public functions. For example, call/deploy
plans and view preparation are private, and several handlers return `()` after
printing. Extract those selected paths rather than call command handlers and
parse their human output.

Return existing typed reports where available. Small internal result structs are
appropriate where rendering currently consumes local variables or `json!` data.
They do not need a new public schema. Preserve existing `CommandResult` payloads
and versions; UI progress events are internal, not a new JSON stream contract.

A small internal progress/output sink can retain `Print` as the text adapter and
send TUI progress events. Blocking `Print::ask`/`confirm` must not execute inside
raw mode. Transaction review is explicit UI state, followed by the shared action.
Do not make every command generic or refactor deferred command modules to meet
an abstraction they do not need.

### Runtime and work ownership

Use one TUI-owned Tokio runtime for async RPC and node work. Refactored async
operations use that runtime; existing standalone command wrappers may retain
their blocking adapters.

Run synchronous compilation/metadata work outside the UI thread. Short-lived
blocking helpers can use `spawn_blocking`, but cancelling its join handle does
not stop the work. Long-running subprocess operations need a cancellation path
that reaches their child processes. Do not call nested `Runtime::block_on` on a
Tokio worker or put blocking work in the draw function.

Only one foreground operation runs at a time. Disable competing action controls
with a visible reason; do not queue jobs. Viewing already-loaded results and
scrolling remain available. Refresh and balance/transaction lookups use the same
operation slot. The owned node continues independently.

## 7. Output, cancellation, and local node lifecycle

### Single terminal writer

While raw mode is active, only the TUI renderer writes to the terminal. Route
included commands' progress, command traces, Cargo/test stdout and stderr, and
tracing events to the UI. Pipe and concurrently drain both child output streams.
The current direct writes in `capture_cargo_stderr` and test `write_stream` must
not run against the physical terminal in TUI mode.

Use plain captured subprocess text for v1; request no ANSI color where supported
and sanitize remaining terminal-control sequences before display. Do not forward
child cursor commands to the outer terminal or add an embedded PTY emulator.
Preserve the existing separation between public key information and private
signing material. UI state/history stores public results, not signer objects or
private keys; signing ownership remains in the shared operation layer.

Use a bounded event channel and bounded display buffers. Initial UI limits are
10,000 lines or 2 MiB per visible output buffer, whichever is reached first, and
20 result summaries. Evict old log content with a visible truncation marker.
Log floods may drop/coalesce display updates; completion, errors, and transaction
identity must still be delivered. Drain child pipes even when the UI log is full.

These are UI retention limits. Do not truncate the existing test parser's input
or change public report contents to meet them. Do not retain duplicate full raw
logs inside session history; keep summaries and the latest detailed result per
workflow. Existing large-report capture is not a new bounded-memory guarantee.

Manual scrolling pauses log following; new output does not move the user's
viewport. End resumes following. Draw on changes with a modest rate cap, such as
30 frames per second; idle screens need no continuous full-speed redraw.

### Cancellation and exit

- Before signing/submission, cancellation stops the action without submitting.
- During submission, cancellation cannot establish whether admission occurred.
  Preserve the canonical hash where available and show the existing ambiguous
  outcome diagnostics. No automatic resubmission.
- During receipt polling, cancellation stops waiting. It does not reverse the
  transaction. The retained hash can be inspected through Activity.
- Build/test cancellation must signal the owned process group on supported Unix
  targets, drain output, and reap the child. Use a bounded grace period followed
  by forced termination when needed. The shared runner must retain ordinary CLI
  Ctrl-C behaviour; TUI raw-mode key handling must explicitly request cancellation.
- Pure in-process work stops at safe stage boundaries. Show “Cancelling” until
  cleanup completes; do not claim immediate cancellation of an uninterruptible step.
- Cancelling node acquisition/startup must either prevent startup or stop any
  child that was created and perform matching runtime-state cleanup. Dropping an
  async task is not a substitute for the existing lifecycle cleanup.
- Quitting with an active operation offers Return or Cancel and Exit. With an
  owned node, the exit action explicitly says it will stop that node.
- OS termination signals enter the same cleanup path. Restore cursor, raw mode,
  and alternate-screen state on normal exit, startup failure, and panic. A panic
  hook restores the terminal before printing diagnostics; it is not a promise of
  recovery from forced process termination.
- Normal user exit returns 0 even if a completed job failed. A fatal TUI startup,
  rendering, or cleanup failure returns nonzero after restoring the terminal.

### One owned local node

Start is available for a local profile and is always explicit. The form exposes
the port (default 9545) and an optional data directory. Omission keeps the
existing ephemeral default; label it clearly. Bind to loopback in v1.

Reuse managed toolchain acquisition, environment overrides, bundle identity,
installation leases, PID checks, readiness, and supervisor shutdown. Show
Acquiring, Starting, Running, Stopping, or Failed states based on actual progress.
Preserve existing timeout and failure messages.

`start_local_node()` alone does not perform all CLI lifecycle work. The current
`run_hogd_dev` also publishes local runtime state, handles publication failure,
and clears matching state on exit. Extract/reuse that orchestration for the TUI:

1. Resolve/acquire and start through the existing supervisor.
2. Publish the same local runtime state, including runtime-manifest information.
3. If publication fails, stop the new node and report the failure.
4. Re-resolve the selected profile and show the effective endpoint. If explicit
   environment/config overrides still target another endpoint, show that fact;
   do not imply the running node is the transaction target.
5. Keep ownership in one async task and forward lifecycle/log events to the UI.
6. On stop, unexpected exit, or workspace shutdown, use supervisor teardown and
   conditional runtime-state cleanup. Never clear another process's newer state.

Stop is enabled only for the node this TUI started, and only after a foreground
operation has finished or been cancelled. Starting also uses the foreground slot
until readiness. Navigating away from Overview does not stop a running node.

An externally started node remains externally owned. The TUI can use its
configured RPC endpoint, but cannot stop it, attach to its logs, or adopt its
process. Port/PID conflicts surface existing errors rather than killing another
node. There is no detach, restart orchestration, reset, or chain archive action.

Initialize tracing once for the TUI with a UI sink. Reuse the supervisor's existing
structured child-log bridge; do not call the standalone stderr tracing setup.

## 8. Dependency decisions

- Ratatui owns layout and rendering; Crossterm owns terminal events and modes.
- Start with Rat-Widget/Rat-Focus for input, scrolling, and focus where they reduce
  custom code. Use one focus/event-routing owner throughout the interface.
- Ratcn is an optional source of themed widgets, not a required runtime. Do not
  combine its interactive runtime with Rat-Focus unless ownership is explicit.
  Prefer a paint-only component or defer it when integration adds complexity.
- Rat-Salsa is not required: this MVP already needs a small Tokio event loop, and
  does not need a second application framework.
- Pin compatible released crate versions against the workspace's stable Rust
  toolchain. Verify the selected widget crates support the chosen Ratatui version.
  Avoid duplicate incompatible widget/backend versions.
- Use one small theme with terminal-compatible colors and monochrome support.
  Animation libraries, theme editors, and a component-copy CLI are not v1 needs.

This keeps the selected stack small. Approval of the ecosystem is not a
requirement to adopt every library in it.

## 9. Delivery sequence

Each slice must leave direct CLI commands and JSON output working.

1. **Terminal shell and Overview:** add `tui` dispatch, terminal guard, event loop,
   context selection, read-only findings, responsive layout, and focus. Validate
   widget compatibility and small-screen behaviour here.
2. **Build & Test:** route output into the UI, expose existing result reports,
   implement cancellation/child cleanup, and retain results. This proves the
   execution/presentation boundary before transaction work.
3. **Local node:** reuse full startup/publication/shutdown orchestration and route
   logs through tracing. Prove owned versus external-node behaviour.
4. **Contract loop and Activity:** expose preparation/results for deploy, call,
   and view; add argument entry, existing confirmations, balance, and transaction
   lookup. Preserve partial-success and ambiguous-outcome handling.
5. **Release checks and guidance:** complete terminal/integration tests, update
   CLI help/completions and the canonical knowledge bundle, and check packaging.

If a slice requires a new platform service, protocol, or generic framework,
reduce the UI scope instead of expanding this MVP. Deferred commands are not
hidden acceptance requirements.

## 10. Verification and release acceptance

### Behaviour tests

- Existing command JSON/exit-code/confirmation tests still pass after extraction.
- UI actions and CLI wrappers use the same operation functions and validators.
- Test failure remains a structured result. Declined reviews never sign/submit.
- Deploy alias-save failure shows on-chain success plus the local warning.
- Submission uncertainty and stopped polling retain the hash and do not resubmit.
- Profile changes and config refresh cannot apply late results to a new context.
- Argument encoding is the same for CLI strings and TUI field values, including
  structured values, explicit entrypoints, invalid input, and zero-argument methods.

### Rendering and input tests

Use Ratatui's in-memory backend for representative screens at `120x40`, `80x24`,
`60x18`, `40x12`, and below the minimum. Include long paths/hashes, multiline
errors, empty lists, wide/combining characters, monochrome, and resize during a form.
Assert that controls and full-value details are reachable, not only that a screen
matches a snapshot.

Use pseudo-terminal process tests for raw-mode setup/restore, keyboard navigation,
paste, live resize, cancellation, panic/error exit, and high-volume child output.
Fake Cargo/RPC/node fixtures should prove cleanup and event behaviour without a
live network. Build cancellation must not leave a child or Cargo lock behind.

### End-to-end acceptance

With a configured sample project and supported devnode, a reviewer can:

1. Open `groundhog tui` and identify project, package, profile, endpoint, and wallet.
2. Start a local node and see its actual readiness and persistence mode.
3. Build and run one package's RV32 tests; inspect failures and rerun failed tests.
4. Deploy, see the resulting address/alias, run a view and a signed call, and
   inspect the receipt, return/abort data, and available events.
5. Read a wallet balance and look up a transaction hash without leaving the TUI.
6. Resize throughout the workflow without losing input or drawing outside the screen.
7. Cancel a build or receipt wait with the correct semantics.
8. Exit cleanly, stopping an owned node while leaving an external node untouched.

Check Linux and macOS terminal behaviour on the product's supported release
targets, including a tmux pane. This release does not add a Windows support promise.

Run targeted CLI and lifecycle tests during each slice. Before release, run
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`, plus the relevant live local-node smoke when available.
Update the source knowledge records for `groundhog --skill`, regenerate/check the
bundle, and run its required tests. Do not hand-edit generated guidance.

## 11. Source references

Paths below are relative to the repository root. Current code takes precedence
over this dated inventory if the CLI changes before implementation.

- Command definitions, options, and dispatch: `crates/tools/cli/src/main.rs`.
- Human output and current width fallbacks: `crates/tools/cli/src/{print,terminal,init}.rs`.
- Cargo output/process handling: `crates/tools/cli/src/{lib,build,test_cmd}.rs`.
- Status and config: `crates/tools/cli/src/checks.rs`, `config_cmd.rs`, and `config/`.
- Invocation and results: `crates/tools/cli/src/{call,deploy,view,tx,balance}.rs`.
- Existing test/report types: `crates/tools/cli/src/{test_json,tx_json,view_json,result}.rs`.
- Metadata/codec adapters: `crates/tools/cli/src/contract_spec.rs` and `codec/`.
- Embeddable invocation operations: `crates/client/src/lib.rs`.
- Node lifecycle/publication: `crates/tools/cli/src/node.rs` and `crates/dev/node-supervisor/`.
- Existing process-level CLI tests: `crates/tools/cli/tests/cli_integration_test.rs`.
- [Ratatui](https://ratatui.rs/), [Crossterm](https://github.com/crossterm-rs/crossterm),
  [Rat-Widget/Rat-Focus](https://github.com/thscharler/rat-salsa), and
  [Ratcn](https://github.com/kristoferlund/ratcn).
