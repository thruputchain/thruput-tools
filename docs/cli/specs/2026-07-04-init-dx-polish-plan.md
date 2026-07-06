# Groundhog Init DX Polish Plan
**Date:** 2026-07-04
## Goal
Make `groundhog init` feel like a polished first-run wizard without adding a heavy terminal UI framework.

`groundhog init` is often a user's first Groundhog command. It should welcome the user, show a clear setup path, ask direct questions, and end with a useful review and next steps.

The output should feel designed, not like a stream of status logs.
## Non-Goals
- Do not add spinners, progress bars, or async terminal UI.
  
- Do not add a large ASCII banner yet.
  
- Do not change setup behavior or credential storage.
  
- Do not introduce a dependency-heavy TUI or prompt framework.
  
- Do not make CI or `--json` output more complex.
  
- Do not require ANSI control sequences in non-TTY output.
  
## Proposed Experience
Use a hybrid wizard:

- In an interactive TTY, show a distinct heading, color, a compact progress panel, direct prompts, and a styled completion review.
  
- In non-TTY output, use a readable plain-text fallback without redraws or control sequences.
  
- In `--json`, keep delegating to `status --json`.
  
- In `--quiet`, keep output suppressed.
  
### Interactive TTY Sketch
```text
╭─ Groundhog Setup ─────────────────────────────╮
│ Set up your local development environment.     │
╰───────────────────────────────────────────────╯

Progress
  1. Environment        done
  2. Profiles           done
  3. Signing key        current
  4. Registry access    pending
  5. Project            pending

No signing key found.

? Generate a local signing key now? [Y/n]
```

When a step completes, update the progress panel. If the terminal supports it, redraw the progress panel in place instead of appending another copy. If redraw is not safe, append a compact completed line.
### Plain Fallback Sketch
```text
Groundhog Setup
Set up your local development environment.

[1/5] Environment
[ok] Cargo registry configured
- wasm-opt not installed, optional for build --optimize

[2/5] Profiles
[ok] Config: ~/.config/groundhog/config.toml
[ok] Default profile: local
[ok] Local RPC: http://127.0.0.1:9545

[3/5] Signing key
No signing key found.
Generate a local signing key now? [Y/n]
```
### Completion Review
```text
╭─ Setup Complete ──────────────────────────────╮
│ Config      ~/.config/groundhog/config.toml   │
│ Profile     local                             │
│ Key         default                           │
│ Local RPC   http://127.0.0.1:9545             │
╰───────────────────────────────────────────────╯

Next:
  groundhog new my-contract
  cd my-contract
  groundhog build
```

The plain fallback should contain the same information without boxes or color.
## Visual Direction
- Use a real heading for `Groundhog Setup`.
  
- Use color in TTY mode:
  
  - heading: accent color
    
  - current step: accent color
    
  - completed step: green
    
  - pending step: dim
    
  - warnings/optional notes: yellow or dim
    
- Prefer a compact progress panel over a long list of repeated log lines.
  
- Avoid noisy decoration. The UI should feel calm and practical.
  
- Use Unicode box drawing only in TTY mode. Keep non-TTY output ASCII-safe.
  
## Step Model
The wizard has five steps:

1. Environment
  
2. Profiles
  
3. Signing key
  
4. Registry access
  
5. Project
  

Each step should move through `pending`, `current`, `done`, or `skipped`.

The TTY UI may redraw the progress panel between steps. The fallback UI should append each step once.
## Prompt Copy
Prompts should be direct:

```text
Generate a local signing key now? [Y/n]
Save test executor credentials now? [Y/n]
Create a starter project? [y/N]
Project name:
```

Avoid nested setup language like `Step 1` and `Step 2` inside `auth::login` when called from `init`. The registry token and executor credential prompts should feel like part of the same setup flow.
## Implementation Steps
1. Replace ad hoc init printing with a small local `InitUi` helper.
  

Responsibilities:

- render the heading
  
- render or redraw progress
  
- render success, notes, and skipped states
  
- render completion review
  

2. Add TTY-aware rendering.
  

- Use styled/color output only when stderr is a TTY.
  
- Use plain ASCII fallback otherwise.
  
- Do not emit ANSI redraw sequences in tests, redirected output, or CI.
  

3. Keep command behavior unchanged.
  

- Preserve setup order.
  
- Preserve `--json` behavior.
  
- Preserve `--quiet` behavior.
  

4. Make `auth::login` usable inside the init wizard.
  

- Avoid duplicate headers and nested `Step 1` / `Step 2` copy when called from `init`.
  
- Keep standalone `groundhog login` clear and useful.
  

5. Add a styled completion review.
  

Include:

- config path
  
- default profile
  
- default key if configured
  
- local RPC URL
  
- suggested next commands
  

6. Keep tests focused on behavior and fallback output.
  

- Test that `init` shows the setup heading, steps, completion review, and next commands in non-TTY output.
  
- Avoid brittle ANSI snapshot tests unless the rendering helper is easy to test in isolation.
  
## Acceptance Criteria
- Interactive `groundhog init` has a clear boxed heading, colored progress, and a styled completion review.
  
- Progress does not read like a raw log stream in TTY mode.
  
- Non-TTY output remains readable, plain, and deterministic.
  
- Existing prompts still work in non-interactive and interactive terminals.
  
- `groundhog init --json` output is unchanged.
  
- `groundhog init --quiet` remains quiet.
  
- `groundhog login` remains clear as a standalone command.
  
- Tests cover the fallback human-output shape where practical.
  
- `cargo fmt --all --check`, `cargo test -p groundhog-cli`, and `cargo clippy -p groundhog-cli --all-targets -- -D warnings` pass.
  
## Future Ideas
- Optional ASCII logo once the text flow is stable.
  
- Better token-login flow when registry auth is available through a browser or device-code flow.
  
- A reusable prompt helper if more commands become interactive.
  
- More polished terminal capability detection if other commands adopt styled interactive flows.

## Follow-Up: CLI-Wide Output Polish

`groundhog init` now sets the visual direction for the CLI. Extend the same
styling to other human-facing commands without changing command behavior,
structured JSON, or stdout data output.

The goal is consistency, not a full TUI. Most commands need only a short loading
line and a styled success line.

### Shared Output Helpers

Move reusable styling into `Print` so commands do not duplicate ANSI constants or
terminal checks.

Suggested helpers:

```rust
print.success("Created wallet `alice`");
print.note("wasm-opt not installed, optional for build --optimize");
print.subheading("Registry token");
print.detail("wallet", wallet_address);

let task = print.start_task(format_args!("Creating wallet `{name}`"));
// work
task.success(format_args!("Created wallet `{name}`"));
```

Rules:

- Style only human stderr output.
  
- Keep stdout data output unchanged.
  
- Keep `--json` output unchanged.
  
- Keep `--quiet` quiet.
  
- Use color only when stderr is a TTY.
  
- Use deterministic ASCII fallback in non-TTY output, for example
  `[ok] Created wallet alice`.
  
- Do not hide loading messages when Cargo, tests, node logs, or other subprocess
  output may appear between start and finish.
  
- Avoid spinners, timers, progress bars, async UI, or dependencies that pull the
  CLI toward a TUI framework.

### Phase 1: Simple Mutation Commands

Start with commands where output is simple and no large subprocess output is
expected between the loading line and success line.

Commands:

1. `groundhog new`
  
  - Loading: `Creating project my-contract`
    
  - Success: `Created project my-contract`
    
  - Details: path, template, config path
    
2. `groundhog keys generate`
  
  - Loading: `Generating key default`
    
  - Success: `Generated key default`
    
  - Details: address
    
  - Note: `Default key is now default` when applicable
    
3. `groundhog keys use`
  
  - Success: `Default key set to alice`
    
4. `groundhog keys rm`
  
  - Keep the confirmation prompt plain and direct.
    
  - Success: `Removed key alice`
    
  - Note: `Default key selection cleared` when applicable
    
5. `groundhog wallet create`
  
  - Loading: `Creating wallet alice`
    
  - Success: `Created wallet alice`
    
  - Details: wallet address, transaction hash
    
6. `groundhog wallet rm`
  
  - Success: `Removed wallet alice`
    
7. `groundhog airdrop`
  
  - Loading: `Sending 100 HOG to alice`
    
  - Success: `Airdrop sent`
    
  - Details: transaction hash

Phase 1 should also include shared tests for `Print` fallback behavior and at
least one integration test proving a styled command still keeps JSON and quiet
behavior unchanged.

### Phase 2: Transaction and Read Commands

After Phase 1 output looks right, apply the same primitives to commands with
more detailed result output or network reads.

Commands:

1. `groundhog deploy`
  
  - Loading: `Deploying rv32 artifact` or `Deploying wasm artifact`
    
  - Success: `Deployed contract`
    
  - Details: address, transaction hash, height, saved alias
    
  - Reverted or failed execution remains a warning.
    
2. `groundhog call`
  
  - Loading: `Calling transfer`
    
  - Success: `Call succeeded`
    
  - Details: return value if available, contract, transaction hash, height
    
  - Reverts and faults remain warnings with existing diagnostics.
    
3. `groundhog storage`
  
  - Loading: `Reading storage`
    
  - Success: `Storage read complete`
    
  - Details: address, key, height, item
    
4. `groundhog balance`
  
  - This is data-oriented. Prefer light formatting only.
    
  - Possible success heading: `Balance`
    
  - Details: address and HOG balance
    
5. `groundhog view`
  
  - This is data-oriented and may print events/details.
    
  - Keep existing content structure.
    
  - Consider styling only the initial `Viewing...` line and final result label.
    
6. `groundhog codegen ts --output`
  
  - Success: `Wrote TypeScript bindings`
    
  - Detail: output path

Commands to treat separately:

- `groundhog build`
  
  Keep multi-step build logs. Style the final success line first. Avoid hiding
  loading messages because Cargo output may appear between steps.
  
- `groundhog test`
  
  Preserve harness output, rerun output, watch behavior, and failure summaries.
  Style only high-level lifecycle and final pass/fail lines if doing so is low
  risk.
  
- `groundhog node`
  
  Add a styled startup line such as `Starting Groundhog local node`, then a
  styled ready line with details. Keep forwarded logs plain or dim. Do not change
  tracing semantics in the first pass.
  
- `groundhog status`, `keys ls`, `wallet ls`, and `codegen ts` without
  `--output`
  
  These are table/data or stdout-producing commands. Avoid decorative success
  lines unless the output is redesigned deliberately.

### Acceptance Criteria for CLI-Wide Polish

- Commands still reserve stdout for data or passthrough output where they do so
  today.
  
- `--json` output is byte-for-byte semantically unchanged except for intentional
  payload additions, if any are explicitly approved.
  
- `--quiet` suppresses all human status output.
  
- Non-TTY output is deterministic and readable in tests and logs.
  
- TTY output uses the same green success marker and restrained color language as
  `groundhog init`.
  
- No command gains a spinner or long-lived terminal UI.
  
- Each phase passes `cargo fmt --all --check`, `cargo test -p groundhog-cli`, and
  `cargo clippy -p groundhog-cli --all-targets -- -D warnings`.
