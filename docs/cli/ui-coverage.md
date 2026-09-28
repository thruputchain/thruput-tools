# CLI UI coverage — GH-388

Implementation and verification record for draft PR #443. The command matrix in
`../specs/2026-09-21-cli-tui-mvp.md` defines the scope. JSON, exit codes, confirmation
rules, and raw payloads remain command contracts.

## Family review

| Issue | Commands | Implementation | Verification |
| --- | --- | --- | --- |
| GH-389 | status, shared components | Implemented; user reviewed | Component and CLI tests |
| GH-390 | build, shared progress | Implemented; user reviewed | Progress PTY and build tests |
| GH-391 | queries, history, listings | Shared query cards, grouped history, stdout toolchain table; raw `which` path retained | Workspace tests passed; RPC PTYs cover absent storage, pending transactions, sparse pages, empty events/transfers; command matrix verified |
| GH-416 | config/key/wallet management | Shared mutation/no-change/cancelled results; raw key export retained | Workspace suite and management PTY scenarios passed; existing authorization/secret tests retained |
| GH-417 | toolchain actions, archives, codegen | Shared acquisition/archive progress and action results; raw codegen/which output retained | Workspace suite and archive round-trip/codegen/raw-source/clean-no-op PTYs passed |
| GH-396 | new, init | Shared setup cards, outcomes, next steps; removed fixed ANSI boxes and screen clearing | Workspace suite, counter/stub/ready-init PTYs and declined-step scrollback test passed |
| GH-393 | deploy, call, transfer, airdrop, wallet create | Shared review/dry-run/live/error/unconfirmed results; prepared hashes and prefund progress retained; explicit wallet binding partial outcome | Workspace suite; live call/deploy/transfer success, rejection and uncertainty PTYs; wallet prefund/creation partial failure; remote dry runs without service contact |
| GH-392 | test: single/workspace/watch/integration | Shared per-run/workspace summaries and outcomes; foreground default runs collect passing logs and retain failed output; `--show-output`, no-capture, passthrough, and integration supervisors use append-only child output | Workspace suite, existing watch/JSONL/workspace/integration tests, and compact/failure/full-output PTYs passed |
| GH-395 | node, docs | Shared Ready/stopped reports; docs startup progress; node uses append-only phases/tracing to avoid competing supervisor writers; `--verbose` expands logs | Workspace suite and service PTYs passed, including ready-but-still-running and signal-driven cleanup; logs use shared semantic colors |
| GH-394 | recursive command/mode audit, raw/framework output | Complete local command audit; macOS smoke workflow added | Linux workspace and tmux checks passed; macOS CI result tracked in PR #443 |
| GH-438 | pre-dispatch version warnings and argument errors | Audit gap fixed with shared warnings and RGB error styles | All 43 argument-error paths checked in a terminal, including no UI; version/NO_COLOR/raw-payload tests passed |

## Review rules

- Use only isolated fixtures and local RPC servers for mutation tests.
- Cover normal, empty, invalid, operational error, and applicable partial/cancelled
  outcomes. Review rendered terminal content, not only process exit codes.
- Exercise 24/40/60/80/120-column layouts, no color, ASCII, quiet, JSON, no UI,
  and redirected streams through shared component and command tests.
- Keep secret exports, generated source, completion scripts, and executable paths
  raw. Services and child-terminal passthrough have explicit lifecycle boundaries.
- Record unavailable platform checks honestly; Linux execution is not macOS proof.

## Command-by-command record

All 43 leaf definitions are checked against the specification by
`command_matrix_covers_every_leaf_command`. The binary audit executes `--help`,
terminal argument errors, and `--no-ui` errors for every row. The evidence below
adds command-specific success, error, state, stream, or lifecycle coverage. This
is a branch/mode audit, not a claim to test every possible flag combination.

Test files are under `crates/tools/cli/tests/`. `CLI` below means
`cli_integration_test.rs`, including its `support/ui_audit.rs` fixtures.
`PTY` means `listing_ui_test.rs` or `progress_ui_test.rs` and their support modules.

| Command | Human presentation / contract | Additional verification |
| --- | --- | --- |
| `status` | Shared cards and findings; quiet findings stay silent | CLI status modes, `status_test`, component widths |
| `build` | Compact progress, artifact card, deferred compiler diagnostics; verbose logs | `build_test`, progress PTYs, signals/resize/UTF-8/failure/raw modes |
| `test` | Per-run/workspace results; compact default, full-output handoff modes | `test_cmd_test`, CLI watch/workspace/integration, compact/failure PTYs |
| `deploy` | Review, dry run, receipt/failure results; alias-save warning | CLI deploy variants, live success/failure PTYs |
| `call` | Review, dry run, decoded/raw receipt and diagnostics | CLI call variants, live success/failure PTYs |
| `transfer` | Review, dry run, committed/rejected/unconfirmed results | CLI transfer variants and all three outcome PTYs |
| `airdrop` | Local/remote review and result; retained attempt rules | CLI and library local/remote tests; remote dry-run PTY |
| `wallet create` | Review, dry run, prefund/creation hashes, partial binding result | CLI/library local/remote tests; live success and prefund partial-failure PTYs |
| `view` | Return/abort/fault cards, complete events and decode warnings | CLI view schemas, named/raw/failure/progress PTYs |
| `balance` | Exact HOG/raw values, target and query context | CLI resolution/JSON, zero/u128-max/resize/RPC-error PTYs |
| `storage` | Item or explicit absent result | CLI storage schema/validation, absent-item PTY |
| `tx show` | Status and receipt details, no invented finality | CLI status/receipt/abort variants, pending PTY |
| `tx list` | Page/cursor details, root-history warning below result | CLI sparse/continuation/limit/errors, sparse-page PTY |
| `events list` | Raw/decoded events and continuation context | CLI filters/spec/malformed/empty cases, empty-page PTY |
| `token transfers` | Base-unit amounts, participants, malformed data | CLI mint/burn/filter/continuation/error cases, empty-page PTY |
| `config get` | Persisted values and optional origins on stdout | CLI merged/source/error/JSON cases; multi-width PTYs |
| `config profiles` | Active/missing/invalid states and findings | CLI profile cases, invalid/missing/empty PTYs |
| `keys ls` | Public addresses/default markers on stdout | CLI/`keys_test`, multi-width/plain/quiet PTYs |
| `wallet ls` | Profile, defaults, owner or not-recorded value | CLI wallet cases, multi-width/empty/missing-owner PTYs |
| `toolchain list` | Catalog/install/selection/path table on stdout | `toolchain_test`, CLI stale/cache status coverage |
| `toolchain which` | Raw executable path | `toolchain_test`, cache/path/quiet audit fixture |
| `new` | Project card, warnings, next steps | `new_init_test`, CLI validation, counter/stub PTYs |
| `init` | Append-only setup steps, existing prompts, partial/ready results | CLI EOF/default/quiet/JSON; decline/scrollback/width PTYs |
| `config new` | Created-profile result | CLI atomic/scope/default validation, mutation PTY |
| `config set` | Changed/no-change result | CLI typed-field/scope/JSON cases, mutation PTY |
| `config unset` | Removed/no-change result and inherited fallback | CLI inheritance/scope cases, mutation/no-op PTY |
| `config use` | Saved selection with environment-override warning | CLI selection cases, mutation PTY |
| `config rm` | Existing confirmation and removal result | CLI force/decline/default cleanup, mutation PTY |
| `keys generate` | Public result and default state | CLI duplicate/invalid/JSON cases, generation PTY |
| `keys use` | Default-selection result | CLI/`keys_test`, mutation PTY |
| `keys rm` | Existing confirmation and cleanup result | CLI force/decline/default cleanup, mutation PTY |
| `keys export` | Raw secret only after existing authorization | CLI authorization tests, exact raw/JSON payload audit |
| `wallet use` | Validated default-selection result | CLI ownership/selection cases, selection PTY |
| `wallet rm` | Existing confirmation and inheritance warning | CLI inherited/default/missing cases, removal PTY |
| `toolchain install` | Acquisition progress or cached/no-change result | `toolchain_test` download/offline/locks; cached install PTY |
| `toolchain use` | Changed/no-change selected version | `toolchain_test`, selected-version PTY |
| `toolchain clean` | Removed/no-change result | `toolchain_test` busy/missing/retired cases, no-op PTY |
| `chain dump` | Progress and archive metadata | CLI confirmation/JSON, archive tests and round-trip PTY |
| `chain restore` | Progress, restored state, cleanup warning | Archive validation/rollback tests, round-trip PTY |
| `codegen ts` | File result card or raw source | Codegen fixtures, sidecar errors, exact file/stdout PTY comparison |
| `node` | Ready while running, append-only service logs, shutdown result | Supervisor/node lifecycle tests, Ready/signal-cleanup PTY |
| `docs` | Ready while serving, version warning and stopped result | `docs_test`, Ready/signal-cleanup PTY |
| `completion` | Raw scripts or existing JSON envelope | All five shells compared byte-for-byte with JSON scripts |

## Deliberate boundaries

- Normal data on stdout stays there. Redirected/quiet/no-UI representations retain
  the command's existing content and suppression rules.
- Raw key export, raw codegen, completion, executable paths, and help/version/skill
  are unframed. Framework parser errors retain exit code 2 and use the shared red.
- Cargo/test passthrough and service logs remain append-only where another process
  or supervisor owns output. No alternate screen or raw-input wizard was added.
- Node uses shared append-only startup phases instead of animation: asynchronous
  supervisor logs must not compete with cursor updates. Normal logs show warnings
  and errors; `--verbose` or `RUST_LOG` gives fuller detail.
- Test integration/no-capture/show-output/passthrough modes retain child output.
  Default exclusive foreground test runs collect passing logs and retain failures.
- The sole direct `main` error write is the last-resort report of a failed diagnostic
  write. It cannot safely recurse into the renderer that just failed.

## Platform and release checks

- Linux workspace tests, build, all-target Clippy, formatting, and diff checks pass.
- PTYs exercise widths 24/40/60/80/120, short terminals, scrollback, interruption,
  resume, source snippets, warnings, exact values, and output modes.
- An isolated tmux server ran a slow local balance RPC: progress completed, the
  complete result remained in scrollback, and the command exited successfully.
- `.github/workflows/cli-ui.yml` runs shared components, progress/listing PTYs, and
  the command/service audit on the existing macOS ARM runner. Its remote result is
  recorded on draft PR #443; no local macOS execution is claimed.
- Generated guidance and package contents are checked before the final push.
