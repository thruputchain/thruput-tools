# CLI Config Management
**Date:** 2026-07-23 **Status:** Implemented

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
groundhog config new staging --network devnet --rpc-url https://rpc.example --global --use
groundhog config rm staging --global
groundhog config set rpc-url https://rpc.example --profile devnet --global
groundhog config unset faucet-url --profile devnet --project
groundhog config use devnet --global
```

The command gives humans, scripts, and CI deterministic inspection and mutation commands. Profile creation accepts a complete set of initial values in one atomic operation; later changes remain field-oriented.

## Non-goals
This work does not:

- replace profiles with a new network abstraction;
- introduce arbitrary dotted-path TOML editing;
- expose or edit executor credentials, private keys, or local-node process state;
- change signer selection or `dev-0` behavior;
- add profile inheritance beyond the existing global/project merge;
- add tombstones that mask inherited values;
- edit individual contract aliases or wallet bindings;
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

`config get` JSON includes persisted runtime paths, contract aliases, and wallet bindings for context but does not edit them. Human output omits runtime paths. Persisted profile runtime paths remain read-only context; `groundhog node` owns separate top-level local runtime state, while deploy and wallet commands own aliases and wallet bindings.

Global config also contains `[executor]`, the top-level key selected by `groundhog keys use`, and `[local_runtime_state]`. These values are outside this feature. Config output must not expose credentials or raw runtime state.
## Decisions
| #   | Decision | Rationale |
| --- | --- | --- |
| D1  | `config get` shows the persisted global/project merge by default. | The command manages configuration files, not runtime invocation state. |
| D2  | Mutations use `--global` and `--project` to target one physical layer. | A merged profile cannot be written back without destroying layering. |
| D3  | `config new` creates one profile from typed options in one transaction. | Users can create a useful profile without editing TOML or issuing one command per field. |
| D4  | `config set` and `config unset` modify typed profile fields. | Typed fields provide validation, help, completion, and a stable interface. |
| D5  | `config new` owns profile creation; `config set` requires the profile in the selected layer. | Creation and modification have distinct, predictable failure behavior. |
| D6  | `config unset` removes a field only from the selected layer. | An inherited value may become visible; no tombstone is created. |
| D7  | Profile deletion requires confirmation in normal human output mode. | Deletion removes the entire profile table, including aliases and wallet bindings. JSON and quiet modes remain non-interactive for scripts. |
| D8  | All config writers preserve comments and unknown fields and use shared sidecar locks. | Existing commands write the same files and must not erase or race config edits. Cross-layer validation locks both files in canonical order. |
| D9  | Active-profile resolution requires a selected existing profile. | Inventing an empty `default` profile hides missing or invalid configuration. |
## Command Surface
### `groundhog config get [PROFILE]`
Show one profile. With no positional profile, use the existing active-profile selection rules.

```text
--sources    Show where each merged value came from
```

Output is always the persisted global/project merge. It does not include `local_runtime_state`, signer fallback, `GROUNDHOG_RUNTIME_MANIFEST`, or command-specific overrides. `--sources` identifies which layer contributed each effective value.

Human output:

```text
Profile: devnet

Network           devnet
RPC URL           https://rpc.devnet.example
Faucet URL        https://faucet.devnet.example
Default key       alice
Default wallet    not set

Contracts
  HOG              0x0000...0001
  counter          0x18ab...9f23

Wallets
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

Human output omits selection provenance, internal scope, runtime manifest, and runtime artifact fields. JSON retains the reviewed typed inspection model for machine consumers. Merged runtime paths are normalized internally using the config file that supplied each value as the base.
### `groundhog config profiles`
List the sorted union of global and project profile names:

```text
Profiles

    PROFILE   NETWORK
  * local     local
    devnet    devnet
  ! staging   invalid
```

Interactive and piped output use `*` for the active profile and `!` for invalid or missing profiles. Interactive output uses warning color for `!` and dims network values.

The command must:

- mark the active profile;

- show the merged network when valid;

- include an active missing profile as `missing` rather than failing;

- include a schema-invalid profile as `invalid`, show a concise reason, and continue listing valid profiles.


When no source selects a profile, listing still succeeds, marks no row active, and emits `activeProfile: null` with no `selectionSource` in JSON. A TOML syntax error still fails inspection of that file with its path and source location. Once TOML parses, profiles are decoded and validated independently so one invalid profile does not hide the others.
### `groundhog config new <PROFILE>`
Create one profile in one config layer:

```bash
groundhog config new staging \
  --network devnet \
  --rpc-url https://rpc.example \
  --faucet-url https://faucet.example \
  --default-key alice \
  --global \
  --use
```

Options:

```text
--network <NETWORK>       Required: local, devnet, or mainnet
--rpc-url <URL>           Optional absolute HTTP(S) RPC URL
--faucet-url <URL>        Optional absolute HTTP(S) faucet URL
--default-key <NAME>      Optional profile-specific signing key
--use                     Also select the new profile in the target layer
--global                  Create the profile in global config
--project                 Create the profile in project config
```

Scope follows the mutation rules: `--global` and `--project` conflict, a caller inside a project must choose a scope, omitted scope outside a project means global, and `--project` requires a discoverable `groundhog.toml`.

Existence is relative to the selected layer. `config new staging --project` may create a project overlay when `staging` already exists globally. The command fails with `profile_already_exists` when the selected layer already contains that profile. It never overwrites or merges into an existing target-layer profile; use `config set` for later changes.

Parse and validate every supplied value before acquiring the write lock. Inside the lock, reread the file, confirm that the target-layer profile is still absent, insert all supplied fields, validate the complete profile, and replace the file atomically. `--use` writes top-level `default_profile` in the same transaction. Successful creation reports `changed: true`; an existing target-layer profile is an error rather than a no-op.

Warn when `--default-key` names a key not stored on this machine. When `--use` is present, also warn if `GROUNDHOG_PROFILE` still controls active selection.
### `groundhog config rm <PROFILE>`
Remove one complete profile from one config layer:

```bash
groundhog config rm staging --global
```

The command follows the mutation scope rules. Profile existence is relative to the selected layer, so removing a project overlay leaves a same-named global profile intact. A missing target-layer profile fails with `profile_not_found_in_scope`.

Normal human output asks for confirmation and defaults to cancellation. JSON and quiet modes skip the prompt, matching `groundhog keys rm`. If the destination file changes while the prompt is open, deletion fails and asks the user to review and retry. Confirmed deletion removes the complete target-layer profile table, including its scalar fields, contract aliases, wallet bindings, and unknown profile-owned entries. If that layer's `default_profile` names the removed profile, the command clears the selection in the same locked transaction.

Deletion preserves unrelated profiles, top-level fields, comments, credentials, and runtime state. The JSON report includes the profile, scope, destination path, `changed`, and `clearedDefault`.
### `groundhog config set <FIELD> <VALUE>`
Set one typed field in one profile layer:

```bash
groundhog config set network devnet --profile devnet --global
groundhog config set rpc-url https://rpc.example --profile devnet --global
groundhog config set default-key alice --profile devnet --project
```

`--profile <PROFILE>` is required. Mutation commands never infer their target profile from active-profile selection.

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
- the profile must exist in the selected layer;
- successful output names the profile, field, and file that changed.

Parse the typed value before acquiring the write lock. Inside the lock, reread the file, apply the change, validate the edited profile, and replace the file atomically. Setting the current value succeeds with `changed: false`.

If the profile is absent from the selected layer, fail with `profile_not_found_in_scope`. This remains true when the same name exists in the other layer. The hint directs the user to `config new` for the selected layer.
### `groundhog config unset <FIELD>`
Remove one supported field from one profile layer:

```bash
groundhog config unset faucet-url --profile devnet --project
```

`--profile <PROFILE>` is required. `unset` follows the same scope rules as `set`. It removes only the selected TOML key. If a global value exists, it becomes the merged value after removing the project override.

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

`use` does not create profiles. `use --global` requires the profile in global config because that default must work outside the current project. `use --project` accepts a profile from either layer because project commands resolve the merged profile. Scope otherwise follows the same mutation rules.

Selecting the target layer's current default succeeds with `changed: false`. If `GROUNDHOG_PROFILE` is set, update the selected file but warn that the environment variable still controls active selection.
## Validation
Use the same typed field validation for creation and mutation commands:

- trim profile names before validation and storage;
- reject empty profile names and control characters;
- accept only `local`, `devnet`, or `mainnet` for `network`;
- require absolute `http://` or `https://` RPC and faucet URLs;
- require a non-empty configured default-key name;
- allow default-key names not present on this machine, with a warning;
- preserve unknown fields without exposing them through typed editing.

Existing contract and wallet entries remain readable. Config commands do not take ownership of validating or rewriting those maps beyond what is needed to render them safely.

`get`, `new`, `set`, and `unset` validate their target profile. `profiles` validates entries independently. Unrelated invalid profiles do not block inspection or repair.
## Persistence and Concurrency
All global and project config writers use one document-oriented mutation path. It preserves:

- comments and ordering where `toml_edit` supports them;

- unknown fields and unrelated tables;

- existing project-file permissions;

- global mode `0600` on Unix.


The shared path covers:

- `init` profile and credential updates;

- default-key changes from key commands;

- local runtime-state updates from `groundhog node`;

- deploy contract-alias persistence;

- wallet binding persistence.


Each config file uses a stable sidecar lock held across:

```text
fresh read -> parse -> mutate -> validate -> atomic replace
```

Lock acquisition has a bounded wait and returns an actionable error on contention. Cross-layer validation locks canonical global and project paths in deterministic order.

Existing commands keep their current higher-level behavior when a config write fails. This work changes their persistence mechanism, not their transaction or process semantics. In particular, a completed deployment is not reclassified as failed because alias persistence failed.

Writes use same-directory temporary files, sync file contents, rename atomically, and sync the parent directory. Existing project files retain their mode; new project files use normal permissions subject to the process umask. Global config is normalized to `0600` on every write.
## Secrets and Protected State
No config command may print or edit:

- `[executor]` credentials;

- private key material or key-file contents;

- the top-level global default key selected by `groundhog keys use`;

- `[local_runtime_state]`;

- cached network descriptors or pending faucet records.

Build output from allowlisted profile fields. Never serialize the complete global document and redact it afterward.
## Output Contracts
All config commands support human and JSON output.

- human data from `config get` and `config profiles` goes to stdout;
- mutation confirmations and warnings go to stderr;
- JSON uses the existing `CommandResult` envelope on stdout;
- JSON warnings appear in that envelope instead of stderr;
- JSON mode emits no human status text on stdout.

Command names are:

```text
config.get
config.profiles
config.new
config.rm
config.set
config.unset
config.use
```

Each data payload includes `schemaVersion: 1` and camelCase fields.

`config.get` includes the profile, selection source, fixed `resolved` scope, profile values, contracts, wallets, and optional per-value sources. Selection source and scope are machine metadata and do not appear in human output.

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

Unset scalar fields have a `null` value and no source entry. Runtime path values use their normalized merged form.

`config.profiles` includes the active profile, selection source, and sorted rows with layer presence, network, status, and an optional invalid reason.

Mutation results include the profile, scope, destination path, and changed flag. `config.set` and `config.unset` include the field. `config.new` includes sorted `createdFields` and `selected`. `config.rm` includes `clearedDefault`. `createdFields` uses kebab-case CLI names such as `rpc-url` and always includes `network`. `selected` is true when `--use` was requested and the new profile is the target layer's default after the transaction. `config use` and `config new --use` warn when `GROUNDHOG_PROFILE` remains active.
## Errors and Edge Cases
Use stable config-specific errors for:

- invalid or missing profile names;

- a profile that already exists in the selected layer;

- a profile missing from the selected mutation scope;

- no active profile selected;

- unsupported fields or invalid values;

- project config unavailable;

- ambiguous or conflicting scope;

- malformed TOML with path and location;

- lock contention;

- destination changes while a deletion confirmation is open;

- destination write or atomic replacement failure.


Important behavior:

- a profile may exist globally, in the project, or in both;

- `new --project` can create an overlay for a global profile;

- empty maps and unset scalar fields are normal;

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

- `config get`, `config profiles`, `config new`, `config rm`, `config set`, `config unset`, and `config use`;

- stable JSON reports.

## Acceptance Criteria
1. A user can inspect effective persisted profile values without reading TOML.

2. Source output identifies global and project contributions per field and map entry.

3. A user can create a complete profile or project overlay in one command.

4. A user can set a value, remove a stored value or complete profile, and select a default profile.

5. Only profile deletion prompts in normal human output mode; config writes always identify their destination.

6. Profile creation writes all supplied fields, plus optional selection, in one transaction.

7. All config writers preserve unrelated comments and unknown fields.

8. Concurrent config writers do not silently lose updates.

9. No config command changes signer or `dev-0` behavior.

10. No config command exposes credentials, key material, or local runtime state.

11. Human and JSON output follow existing stream and schema conventions.

## Verification
Unit and config tests cover:

- active-profile precedence;
- global/project scalar and per-entry map merging;
- merged profile views;
- per-value provenance;
- every typed field's valid and invalid forms;
- atomic profile creation with required and optional fields;
- target-layer duplicate rejection and project-overlay creation;
- `new --use` selection and environment-override warnings;
- profile deletion confirmation, cancellation, default clearing, selected-layer errors, and concurrent removal;
- set updates, selected-layer missing errors, unchanged values, and missing default-key warnings;
- unset with and without inherited fallback;
- profile listing with valid, invalid, and active-missing entries;
- profile listing without an active selection;
- global/project `use`, unchanged selection, and the environment-override warning;
- comments, unknown fields, ordering, and permissions surviving every writer;
- concurrent writers, lock timeout, and atomic-failure cleanup;
- relative path bases;
- omission of credentials and protected runtime state.

Binary integration tests cover parser behavior, stdout/stderr rules, JSON schemas, stable errors, and real file mutation under isolated XDG directories.
## Deferred Follow-ups
- tombstones that mask inherited values;
- contract alias and wallet binding editing;
- profile import/export and cloning;
- network health checks.
