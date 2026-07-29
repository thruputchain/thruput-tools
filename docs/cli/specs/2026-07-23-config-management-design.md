# CLI Config Management
**Date:** 2026-07-23 **Status:** Design ready for implementation

**Companions:**

- `2026-07-02-global-and-project-config-brief.md` - the global/project profile model exposed by this command.
  
- `../roadmap.md` - current CLI surface and sequencing.
  
## Goal
Let users inspect and edit Groundhog profiles without opening a TOML file.

The common flows should be simple:

```bash
groundhog config get
groundhog config get devnet --sources
groundhog config profiles
groundhog config set rpc-url https://rpc.example --profile devnet --global
groundhog config unset faucet-url --profile devnet --project
groundhog config use devnet --global
groundhog config edit devnet
```

The command serves two audiences:

- humans need a guided editor that explains inherited values and where changes will be saved;
- scripts and CI need deterministic, non-interactive inspection and mutation commands.

## Non-goals
This work does not:

- replace profiles with a new network abstraction;
- introduce arbitrary dotted-path TOML editing;
- expose or edit executor credentials, private keys, or local-node process state;
- change signer selection or `dev-0` behavior;
- add profile inheritance beyond the existing global/project merge;
- add tombstones that mask inherited values;
- edit contract aliases or wallet bindings;
- change profile-selection precedence;
- resolve unrelated project scaffold behavior.

## Current Model
Groundhog has two durable configuration layers:

| Layer | Location | Purpose |
| --- | --- | --- |
| Global | `$XDG_CONFIG_HOME/groundhog/config.toml`, otherwise `~/.config/groundhog/config.toml` | User-wide profiles and defaults |
| Project | Nearest ancestor `groundhog.toml` | Project-specific profile overrides, contract aliases, and wallet bindings |

The selected profile name resolves in this order:

1. explicit `--profile`;
  
2. `GROUNDHOG_PROFILE`;
  
3. project `default_profile`;
  
4. global `default_profile`;

If no source selects a profile, resolution fails with a stable no-active-profile error. A selected name must exist in the global layer, project layer, or both. The name `default` has no special behavior.

For a selected name, global and project profiles merge field by field. Project scalar values override global scalar values. Contract and wallet maps merge per entry, with project entries winning.

The editable profile fields are:

```text
network
rpc_url
faucet_url
default_key
```

`config` shows runtime paths, contract aliases, and wallet bindings for context but does not edit them. `groundhog node`, deploy, and wallet commands continue to own those entries.

Global config also contains `[executor]`, the top-level key selected by `groundhog keys use`, and `[local_runtime_state]`. These values are outside this feature. Config output must not expose credentials or raw runtime state.
## Decisions
| #   | Decision | Rationale |
| --- | --- | --- |
| D1  | `config get` shows the persisted global/project merge by default. | The command manages configuration files, not runtime invocation state. |
| D2  | `--global` and `--project` inspect or mutate one physical layer. | A merged profile cannot be written back without destroying layering. |
| D3  | `config set` and `config unset` expose typed profile fields. | Typed fields provide validation, help, completion, and a stable interface. |
| D4  | `config set` creates a missing profile table in the selected layer. | This avoids a separate profile-creation command. |
| D5  | `config unset` removes a field only from the selected layer. | An inherited value may become visible; no tombstone is created. |
| D6  | `config edit` follows the existing `groundhog init` prompt model. | A line-based wizard is sufficient and requires no TUI dependency. |
| D7  | All config writers preserve comments and unknown fields and share one lock. | Existing commands write the same files and must not erase or race config edits. |
| D8  | Active-profile resolution requires a selected existing profile. | Inventing an empty `default` profile hides missing or invalid configuration. |
## Command Surface
### `groundhog config get [PROFILE]`
Show one profile. With no positional profile, use the existing active-profile selection rules.

```text
--sources    Show where each merged value came from
--global     Show only the global layer
--project    Show only the project layer
```

`--global` and `--project` conflict. Without either flag, output is the persisted global/project merge. It does not include `local_runtime_state`, signer fallback, `GROUNDHOG_RUNTIME_MANIFEST`, or command-specific overrides.

Human output:

```text
Profile: devnet
Selected by: project default

Network:          devnet
RPC URL:          https://rpc.devnet.example
Faucet URL:       https://faucet.devnet.example
Default key:      alice
Runtime manifest: not set

Contracts:
  HOG              0x0000...0001
  counter          0x18ab...9f23

Wallets:
  alice            0x91c2...4d10
```

With `--sources`, show global or project provenance per scalar and map entry:

```text
Field             Value                              Source
network           devnet                             global
rpc_url           https://rpc.company.internal       project
faucet_url        https://faucet.devnet.example      global
default_key       alice                              project
contracts.HOG     0x0000...0001                      global
contracts.counter 0x18ab...9f23                      project
```

Raw layer views show only values stored in that file. They use an allowlisted profile projection rather than printing the whole global document.

Raw layer views preserve runtime paths exactly as stored. Merged output resolves relative runtime paths to absolute paths using the config file that supplied each value as the base. Inspection never rewrites the stored spelling.
### `groundhog config profiles`
List the sorted union of global and project profile names:

```text
PROFILE    ACTIVE    GLOBAL    PROJECT    NETWORK    STATUS
local      yes       yes       yes        local      valid
devnet               yes       yes        devnet     valid
staging              yes       no         -          invalid
```

The command must:

- mark the active profile and how it was selected;
  
- show whether each profile exists globally, in the project, or in both;
  
- show the merged network when valid;
  
- include an active missing profile as `missing` rather than failing;
  
- include a schema-invalid profile as `invalid`, show a concise reason, and continue listing valid profiles.
  

A TOML syntax error still fails inspection of that file with its path and source location. Once TOML parses, profiles are decoded and validated independently so one invalid profile does not hide the others.
### `groundhog config set <FIELD> <VALUE>`
Set one typed field in one profile layer:

```bash
groundhog config set network devnet --profile devnet --global
groundhog config set rpc-url https://rpc.example --profile devnet --global
groundhog config set default-key alice --profile devnet --project
```

Supported fields:

```text
network
rpc-url
faucet-url
default-key
```

Scope rules:

- `--global` and `--project` conflict;
- when a project config exists, the caller must choose a scope;
- outside a project, omitted scope means global;
- `--project` fails when no project config is discoverable;
- the selected profile table is created when absent;
- successful output names the profile, field, and file that changed.

Parse the typed value before acquiring the write lock. Inside the lock, reread the file, apply the change, validate the edited profile, and replace the file atomically. Setting the current value succeeds with `changed: false`.
### `groundhog config unset <FIELD>`
Remove one supported field from one profile layer:

```bash
groundhog config unset faucet-url --profile devnet --project
```

`unset` follows the same scope rules as `set`. It removes only the selected TOML key. If a global value exists, it becomes the merged value after removing the project override.

The command does not remove profile tables, contract aliases, wallet bindings, or top-level defaults. Removing a field that is not stored in the selected layer succeeds with `changed: false`. If the profile table itself is absent from that layer, `unset` is the same no-op and does not create an empty profile.

Human output should explain when the merged value now comes from the other layer.
### `groundhog config use <PROFILE>`
Choose the profile Groundhog uses when a command omits `--profile`:

```bash
groundhog config use devnet --global
groundhog config use local --project
```

The command writes the top-level `default_profile` field; it does not change any fields inside the profile.

- `--global` changes the default outside projects and in projects that do not set their own default;
- `--project` changes the default only for commands run in that project;
- an explicit `--profile` or `GROUNDHOG_PROFILE` still takes precedence.

The target profile must exist in global or project configuration; `use` does not create it. Scope follows the same rules as `set`.

If `GROUNDHOG_PROFILE` is set, update the selected file but warn that the environment variable still controls active selection.
### `groundhog config edit [PROFILE]`
`config edit` is Phase 2. It is a line-based wizard implemented with the same stdin and `Print::prompt` pattern as `groundhog init`. It does not use raw terminal mode, an alternate screen, or a TUI framework.

Options:

```text
--global     Edit the global layer
--project    Edit the project layer
```

If scope is omitted, prompt the user to choose it. Then:

1. show the destination file and current merged profile;
  
2. prompt for each editable scalar, allowing keep, replace, or unset;
  
3. show a summary of staged changes and the resulting merged profile;
  
4. ask for confirmation;
  
5. write all changes once.
  

If the profile does not exist in the selected layer, explain that saving will create an overlay and ask for confirmation before editing.

Cancellation or validation failure leaves the file unchanged. The wizard must not hold the lock while waiting for input. At save time it acquires the lock and rejects the update if the file changed since the session began.

`config edit --json` and `config edit --quiet` fail immediately because the command is interactive. No new terminal UI dependency is needed.
## Validation
Use the same typed field validation for non-interactive commands and the editor:

- trim profile names before validation and storage;
- reject empty profile names and control characters;
- accept only `local`, `devnet`, or `mainnet` for `network`;
- require absolute `http://` or `https://` RPC and faucet URLs;
- require a non-empty configured default-key name;
- allow default-key names not present on this machine, with a warning;
- preserve unknown fields without exposing them through typed editing.

Existing contract and wallet entries remain readable. Config commands do not take ownership of validating or rewriting those maps beyond what is needed to render them safely.

`get`, `set`, `unset`, and `edit` validate their target profile. `profiles` validates entries independently. Unrelated invalid profiles do not block inspection or repair.
## Persistence and Concurrency
Project alias and wallet writes already use `toml_edit`. Global mutations currently deserialize and serialize the whole typed document, which removes comments, formatting, and unknown fields.

Introduce one document-oriented mutation path for global and project config. It must preserve:

- comments and ordering where `toml_edit` supports them;
  
- unknown fields and unrelated tables;
  
- existing project-file permissions;
  
- global mode `0600` on Unix.
  

All existing config writers must use this path, including:

- `init` profile and credential updates;
  
- default-key changes from key commands;
  
- local runtime-state updates from `groundhog node`;
  
- deploy contract-alias persistence;
  
- wallet binding persistence.
  

Use a stable sidecar lock for each config file. Hold it across:

```text
fresh read -> parse -> mutate -> validate -> atomic replace
```

Lock acquisition has a bounded wait and returns an actionable error on contention. The implementation may choose the locking mechanism; it must compile on the repository's supported Linux and macOS targets.

Existing commands keep their current higher-level behavior when a config write fails. This work changes their persistence mechanism, not their transaction or process semantics. In particular, a completed deployment is not reclassified as failed because alias persistence failed.

Writes continue to use same-directory temporary files and atomic rename. Preserve an existing project file's mode; create a new project file using normal permissions subject to the process umask. Normalize global config to `0600` on every write.
## Secrets and Protected State
No config command may print or edit:

- `[executor]` credentials;
  
- private key material or key-file contents;
  
- the top-level global default key selected by `groundhog keys use`;
  
- `[local_runtime_state]`;
  
- cached network descriptors or pending faucet records.

Build output from allowlisted profile fields. Never serialize the complete global document and redact it afterward.
## Output Contracts
All non-interactive commands support human and JSON output.

- human data from `config get` and `config profiles` goes to stdout;
- mutation confirmations and warnings go to stderr;
- JSON uses the existing `CommandResult` envelope on stdout;
- JSON mode emits no human status text on stdout.

Command names are:

```text
config.get
config.profiles
config.set
config.unset
config.use
```

Each data payload includes `schemaVersion: 1` and camelCase fields.

`config.get` includes the profile, selection source, scope (`resolved`, `global`, or `project`), profile values, contracts, wallets, and optional per-value sources.

A source is an object with a stable `kind` and optional location:

```json
{ "kind": "project", "path": "/work/app/groundhog.toml" }
```

Field source kinds are `global` and `project`. Profile-selection source kinds are `explicit`, `environment`, `project`, and `global`; environment selection includes `name: "GROUNDHOG_PROFILE"` and file-backed selection includes `path`.

When `--sources` is used, `config.get` groups source records by the same logical shape as the values:

```json
{
  "sources": {
    "network": { "kind": "global", "path": "/home/user/.config/groundhog/config.toml" },
    "rpcUrl": { "kind": "project", "path": "/work/app/groundhog.toml" },
    "contracts": {
      "HOG": { "kind": "global", "path": "/home/user/.config/groundhog/config.toml" }
    },
    "wallets": {}
  }
}
```

Unset scalar fields have a `null` value and no source entry. Raw layer output uses that layer's source for every stored value. Runtime path values follow the raw-versus-merged rules above.

`config.profiles` includes the active profile, selection source, and sorted rows with layer presence, network, status, and an optional invalid reason.

Mutation results include the profile, scope, destination path, changed flag, and field where applicable. `config use` also warns when `GROUNDHOG_PROFILE` remains active.
## Errors and Edge Cases
Use stable config-specific errors for:

- invalid or missing profile names;

- no active profile selected;
  
- unsupported fields or invalid values;
  
- project config unavailable;
  
- ambiguous or conflicting scope;
  
- malformed TOML with path and location;
  
- lock contention;
  
- concurrent interactive edit;
  
- unsupported interactive JSON/quiet mode;
  
- destination write or atomic replacement failure.
  

Important behavior:

- a profile may exist globally, in the project, or in both;
  
- `set --project` can create an overlay for a global profile;
  
- empty maps and unset scalar fields are normal;
  
- `get --project` outside a project is an error;
  
- environment-selected profiles remain selected until the environment changes;
  
- stale local runtime state remains owned by node lifecycle and status diagnostics;
  
- interruption before atomic rename leaves the original file intact.
  
## Implementation Shape
Keep the work inside `crates/tools/cli`; no RPC or protocol changes are needed.

Expected areas:

- command parsing and dispatch in `src/main.rs`;
  
- a config command module for human and JSON orchestration;
  
- typed fields and validation under `src/config/`;
  
- document-preserving config transactions and locking under `src/config/`;
  
- persisted global/project merge with per-value provenance;
  
- stable JSON report types;
  
- config-specific `CliError` variants and hints.
  

Do not build a second runtime profile resolver. Config inspection may share the existing merge rules, but it deliberately stops before local runtime and signer resolution.
## Phasing
### Phase 1: Non-interactive Foundation
- document-preserving writes and shared locking;
  
- typed validation;
  
- persisted merge provenance;
  
- `config get`, `config profiles`, `config set`, `config unset`, and `config use`;
  
- stable JSON reports.
  
### Phase 2: Guided Editor
- line-based `config edit` using existing prompt conventions;
  
- staged changes and confirmation;
  
- keep, replace, and unset choices;
  
- concurrent-edit detection.
  
## Acceptance Criteria
1. A user can discover persisted global, project, and merged profile values without reading TOML.
  
2. Source output identifies global and project contributions per field and map entry.
  
3. A user can create an overlay, set a value, remove a stored value, and select a default profile.
  
4. Non-interactive writes never prompt and always identify their destination.
  
5. The guided editor uses existing prompt infrastructure and adds no TUI dependency.
  
6. All config writers preserve unrelated comments and unknown fields.
  
7. Concurrent config writers do not silently lose updates.
  
8. No config command changes signer or `dev-0` behavior.
  
9. No config command exposes credentials, key material, or local runtime state.
  
10. Human and JSON output follow existing stream and schema conventions.
  
## Test Plan
Unit and config tests cover:

- active-profile precedence;
- global/project scalar and per-entry map merging;
- resolved, global-only, and project-only views;
- per-value provenance;
- every typed field's valid and invalid forms;
- set-created overlays and unchanged values;
- unset with and without inherited fallback;
- profile listing with valid, invalid, and active-missing entries;
- global/project `use` and the environment-override warning;
- comments, unknown fields, ordering, and permissions surviving every writer;
- concurrent writers, lock timeout, and atomic-failure cleanup;
- relative path bases;
- omission of credentials and protected runtime state.

Binary integration tests cover parser behavior, stdout/stderr rules, JSON schemas, stable errors, and real file mutation under isolated XDG directories.

Phase 2 tests cover the prompt state machine separately from stdin/stdout plumbing, including save, cancel, invalid input, unset, and concurrent-edit rejection.
## Deferred Follow-ups
- profile deletion;
- tombstones that mask inherited values;
- contract alias and wallet binding editing;
- profile import/export and cloning;
- `$EDITOR` integration;
- network health checks.

---
comments:
  c1:
    body: this looks good but i dont understand what "config use" does, please
      clarify that one
    by: user
    at: 2026-07-24T13:19:13.133Z
  c2:
    body: Clarified that `config use` selects the default profile used when
      `--profile` is omitted, and explained global, project, environment, and
      explicit-profile precedence.
    by: OpenCode
    at: 2026-07-24T13:19:53.000Z
    re: c1
  c3:
    body: these two fields are auto written when running groundhog node so i dont
      think we should support manually changing these. lets remove them from
      supported fields list
    by: user
    at: 2026-07-24T13:21:36.000Z
  c4:
    body: Removed runtime manifest and runtime artifacts directory from `set`,
      `unset`, and guided editing. They remain visible as read-only profile
      context, while `groundhog node` owns their runtime management.
    by: OpenCode
    at: 2026-07-24T13:23:01.000Z
    re: c3
