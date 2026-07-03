# Global and Project Config Implementation Plan

**Date:** 2026-07-03
**Source brief:** [`2026-07-02-global-and-project-config-brief.md`](2026-07-02-global-and-project-config-brief.md)

## Goal

Implement merged global and project CLI config so wallet, balance, faucet, and
local-node workflows work from any directory while preserving project-local
overrides for contract development.

## Agreed Behavior

- Global config lives at `~/.config/groundhog/config.toml`.
- Project config lives at `groundhog.toml`, discovered by walking up from cwd.
- Effective config precedence is CLI flags, environment variables, project
  config, global config, built-in defaults.
- Omitted `--profile` resolves through `GROUNDHOG_PROFILE`, project
  `default_profile`, global `default_profile`, then `default`.
- Profiles merge field-by-field. Project values override global values.
- `contracts` and `wallets` merge as maps. Project aliases override global
  aliases.
- `groundhog init` creates or updates global config only.
- `groundhog init` creates `local` and `devnet` profiles and sets
  `default_profile = "local"` on first setup.
- `groundhog new` still creates project `groundhog.toml`, but only as a minimal
  project overlay/example.
- `groundhog node` writes the active profile's local-node `rpc_url` and absolute
  `runtime_manifest`, regardless of profile name.
- When no project config exists, state-writing commands write profile state to
  global config.

## Phase 1: Config Schema And Loading

Add global profile/default-profile support without changing command behavior yet.

Todo:

- Extend the existing global config model with `default_profile` and `profile`.
- Add `default_profile` to the project config model.
- Split config loading into explicit global and project reads.
- Implement active profile name resolution using explicit profile,
  `GROUNDHOG_PROFILE`, project `default_profile`, global `default_profile`, then
  `default`.
- Implement effective profile merging for scalar fields and map fields.
- Preserve current global executor credentials and default key behavior.
- Add unit tests for profile name resolution, field inheritance, map merging,
  and project override behavior.

Verification:

- `cargo test -p groundhog-cli config_test`

## Phase 2: Profile State Writes

Teach profile-writing helpers to choose project or global config based on
discovery.

Todo:

- Add write helpers for global profile contracts and wallets.
- Update wallet save/remove behavior to write project config when present and
  global config otherwise.
- Update contract alias save behavior with the same target selection, unless the
  call site is explicitly project-only.
- Preserve comments and formatting with `toml_edit` where practical.
- Add tests for writes with a project config, writes without a project config,
  and project-over-global override after writes.

Verification:

- `cargo test -p groundhog-cli config_test keys_test cli_integration_test`

## Phase 3: Init Behavior

Make `groundhog init` initialize global profiles instead of project-local config.

Todo:

- Add an idempotent global config initialization helper.
- On first setup, write `default_profile = "local"`.
- Ensure `[profile.local]` and `[profile.devnet]` exist without overwriting
  existing fields.
- Preserve executor credentials, default key, existing profiles, contracts, and
  wallets.
- Keep existing registry/key/status setup behavior unless it conflicts with the
  new global config model.
- Add tests for fresh init and init over an existing global config.

Verification:

- `cargo test -p groundhog-cli new_init_test config_test status_test`

## Phase 4: Project Scaffold Template

Update `groundhog new` so project config is a minimal overlay, not duplicated
global setup.

Todo:

- Replace the `groundhog.toml` template with project-local contract and wallet
  examples for `local` and `devnet`.
- Do not write `default_profile`, RPC URLs, faucet URLs, runtime manifests, or
  default keys in the scaffolded project config.
- Add the project `.gitignore` promised in the brief.
- Update scaffold tests to assert the new template shape.

Verification:

- `cargo test -p groundhog-cli new_init_test scaffold_test`

## Phase 5: Local Node Profile Persistence

Move local-node runtime state from cwd-local `.groundhog/local-node.json` into
the selected profile.

Todo:

- Add profile persistence for `rpc_url` and absolute `runtime_manifest`.
- Update `groundhog node` to resolve the active profile using the new default
  profile rules.
- Persist local-node metadata to project config when present and global config
  otherwise.
- Stop using `.groundhog/local-node.json` as the normal source of truth.
- Decide whether to keep read-only legacy fallback for existing projects during
  this change.
- Add tests for `groundhog node`, omitted `--profile`, custom profile names, and
  no-project global persistence.

Verification:

- `cargo test -p groundhog-cli cli_integration_test status_test test_cmd_test`

## Phase 6: Command Integration Pass

Run through account, faucet, balance, deploy, call, view, storage, and test flows
for config resolution consistency.

Todo:

- Update call sites that assume a project config is required for profile lookup.
- Ensure account-level commands work without cwd-local `groundhog.toml`.
- Ensure project commands still prefer project overrides when a project config is
  present.
- Audit JSON output for profile source/path fields that may now be global.
- Update error messages so missing profile/config guidance mentions global init.
- Add or update integration tests around representative no-project workflows.

Verification:

- `cargo test -p groundhog-cli`

## Phase 7: Documentation And Cleanup

Make user-facing docs match the implemented behavior.

Todo:

- Update CLI docs or help text for global config and profile resolution.
- Update examples that assume `profile.default` in project config.
- Document global/project merge behavior.
- Remove stale comments that describe `.groundhog/local-node.json` as the source
  of truth.
- Run formatting and the focused test suite.

Verification:

- `cargo fmt --all --check`
- `cargo test -p groundhog-cli`

## Suggested Working Order

1. Phase 1: Config Schema And Loading.
2. Phase 4: Project Scaffold Template.
3. Phase 2: Profile State Writes.
4. Phase 3: Init Behavior.
5. Phase 5: Local Node Profile Persistence.
6. Phase 6: Command Integration Pass.
7. Phase 7: Documentation And Cleanup.

This order front-loads the core config model, then updates low-risk scaffold
behavior before touching stateful command flows.
