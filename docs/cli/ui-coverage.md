# CLI UI coverage — GH-388

Implementation and verification record for draft PR #443. The command matrix in
`../specs/2026-09-21-cli-tui-mvp.md` defines the scope. JSON, exit codes, confirmation
rules, and raw payloads remain command contracts.

## Family review

| Issue | Commands | Implementation | Verification |
| --- | --- | --- | --- |
| GH-389 | status, shared components | Implemented; user reviewed | Component and CLI tests |
| GH-390 | build, shared progress | Implemented; user reviewed | Progress PTY and build tests |
| GH-391 | queries, history, listings | Shared query cards, grouped history, stdout toolchain table; raw `which` path retained | Workspace tests passed; RPC PTYs cover absent storage, pending transactions, sparse pages, empty events/transfers; final matrix audit pending |
| GH-416 | config/key/wallet management | Shared mutation/no-change/cancelled results; raw key export retained | Workspace suite and management PTY scenarios passed; existing authorization/secret tests retained |
| GH-417 | toolchain actions, archives, codegen | Shared acquisition/archive progress and action results; raw codegen/which output retained | Workspace suite and archive round-trip/codegen/raw-source/clean-no-op PTYs passed |
| GH-396 | new, init | Shared setup cards, outcomes, next steps; removed fixed ANSI boxes and screen clearing | Workspace suite, counter/stub/ready-init PTYs and declined-step scrollback test passed |
| GH-393 | deploy, call, transfer, airdrop, wallet create | Shared review/dry-run/live/error/unconfirmed results; prepared hashes and prefund progress retained; explicit wallet binding partial outcome | Workspace suite and live call/deploy success/failure PTYs passed; remote dry-run fixtures verify no service contact; final matrix audit pending |
| GH-392 | test: single/workspace/watch/integration | Pending | Pending |
| GH-395 | node, docs | Pending | Pending |
| GH-394 | recursive command/mode audit, raw/framework output | Pending | Pending |

## Review rules

- Use only isolated fixtures and local RPC servers for mutation tests.
- Cover normal, empty, invalid, operational error, and applicable partial/cancelled
  outcomes. Review rendered terminal content, not only process exit codes.
- Exercise 24/40/60/80/120-column layouts, no color, ASCII, quiet, JSON, no UI,
  and redirected streams through shared component and command tests.
- Keep secret exports, generated source, completion scripts, and executable paths
  raw. Services and child-terminal passthrough have explicit lifecycle boundaries.
- Record unavailable platform checks honestly; Linux execution is not macOS proof.
