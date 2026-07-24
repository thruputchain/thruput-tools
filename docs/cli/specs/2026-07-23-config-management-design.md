# CLI Config Management
**Date:** 2026-07-23 **Status:** Design ready for implementation planning

**Companions:**

- `2026-07-02-global-and-project-config-brief.md` - the global/project profile model this work exposes through the CLI.
  
- `../roadmap.md` - current CLI surface and sequencing.
  
## Goal
Let users inspect, select, create, and edit Groundhog profiles without opening a TOML file, while preserving the existing global/project layering model.

The common flows should be obvious:

```bash
groundhog config get
groundhog config get devnet --sources
groundhog config profiles
groundhog config edit devnet
groundhog config set rpc-url https://rpc.example --profile devnet --global
groundhog config create staging --from devnet --global
groundhog config use staging --project
```

The command must serve two audiences:

- humans need a guided editor that explains inherited values and where changes will be saved;
  
- scripts and CI need deterministic, non-interactive `get`, `set`, `create`, and `use` operations.
  
## Non-goals
This work does not:

- replace profiles with a new network abstraction;
  
- introduce arbitrary dotted-path TOML editing;
  
- expose executor credentials, private keys, or local-node process state;
  
- add profile inheritance beyond the existing global/project merge;
  
- add `unset`, profile deletion, or inherited-value tombstones;
  
- change the precedence of `--profile`, `GROUNDHOG_PROFILE`, project defaults, and global defaults;
  
- resolve unrelated drift in the project scaffold's `default_profile` behavior.
  
## Current model and constraints
Groundhog has two durable configuration layers:

| Layer | Location | Purpose |
| --- | --- | --- |
| Global | `$XDG_CONFIG_HOME/groundhog/config.toml`, otherwise `~/.config/groundhog/config.toml` | User-wide profiles, defaults, credentials, and local runtime state |
| Project | Nearest ancestor `groundhog.toml` | Project-specific profile overrides, contract aliases, and wallet bindings |

The selected profile name resolves in this order:

1. explicit `--profile`;
  
2. `GROUNDHOG_PROFILE`;
  
3. project `default_profile`;
  
4. global `default_profile`;
  
5. the built-in `default` fallback.
  

For a selected name, global and project profiles merge field by field. Project scalar values override global scalar values. Contract and wallet maps merge per entry, with project entries winning. The resolver then fills missing local runtime fields from the global `local_runtime_state` record. See `crates/tools/cli/src/config/project.rs`.

This means "show the config Groundhog will use" and "edit a TOML file" are different operations. The CLI must keep them distinct.

The editable profile fields are:

```text
network
rpc_url
faucet_url
default_key
runtime_manifest
runtime_artifacts_dir
```

Contract aliases and wallet bindings are visible through config inspection, but their lifecycle remains owned by deploy and wallet commands in the first release.

Global config also contains `[executor]` credentials and `[local_runtime_state]`. Neither belongs in the config editor. The latter is ephemeral state managed by `groundhog node`, not a durable user preference.
## Decisions
| # | Decision | Rationale |
|---|---|---|
| D1 | `config get` shows the baseline resolved profile by default. | This matches normal profile resolution without pretending to include command-specific target overrides. |
| D2 | Raw layer inspection uses explicit `--global` or `--project`. | Users need to distinguish inherited values from values stored in one file. |
| D3 | Interactive editing is `config edit`, not an interactive mode of `config set`. | `set` must remain safe for scripts and CI. |
| D4 | Writes target a physical layer, and the destination is always shown. | A merged profile cannot be written back without destroying the layering model. |
| D5 | `config set` exposes typed fields, not arbitrary TOML paths. | Typed fields provide validation, help, completion, and a stable public interface. |
| D6 | Resolved inspection reports provenance for profile values and separately reports active consumer overrides. | Persisted values, local runtime enrichment, profile selection, and consumer-specific environment overrides are distinct concepts. |
| D7 | All config writers preserve comments and unknown fields and share one locking protocol. | Adding a safe editor is not useful if `keys use` or `groundhog node` can later rewrite or race the file. |
| D8 | `unset` and profile deletion are deferred. | Removing a project value reveals its inherited global value; true clearing needs explicit tombstone semantics. |
## Command surface
### `groundhog config get [PROFILE]`
Show one profile. With no positional profile, use the existing active-profile selection rules.

Options:

```text
--sources    Include where the profile selection and each value came from
--global     Show only the global layer
--project    Show only the project layer
--json       Use the existing global JSON output mode
```

`--global` and `--project` conflict. Without either flag, output is the baseline resolved profile: global/project merge plus applicable `local_runtime_state`, using the profile name selected by `GROUNDHOG_PROFILE` when set. It does not apply command-specific flags or consumer-specific environment overrides such as `GROUNDHOG_RUNTIME_MANIFEST`. Active consumer-specific overrides are listed in a separate `Overrides` section so users can see that a later command may replace a baseline value without confusing that override with persisted profile data.

Default-key output distinguishes:

- `Profile default key`: the merged profile's editable `default_key` field;
  
- `Effective signing key`: profile default, otherwise the global key selected by `groundhog keys use`, otherwise the valid local `dev-0` fallback.
  

`config set default-key` edits the profile field. The existing `keys use` command remains the owner of the top-level global key selection.

Human output should prioritize the values users act on:

```text
Profile: devnet
Selected by: project default

Network:          devnet
RPC URL:          https://rpc.devnet.example
Faucet URL:       https://faucet.devnet.example
Profile default:  alice
Effective key:    alice
Runtime manifest: not set

Contracts:
  HOG              0x0000...0001
  counter          0x18ab...9f23

Wallets:
  alice            0x91c2...4d10
```

With `--sources`, show provenance per scalar and map entry:

```text
Field             Effective value                  Source
network           devnet                           global
rpc_url           https://rpc.company.internal     project
faucet_url        https://faucet.devnet.example    global
default_key       alice                            project
effective_key     alice                            project
contracts.HOG     0x0000...0001                    global
contracts.counter 0x18ab...9f23                    project
```

Raw layer views are safe projections of known profile data. They must not dump the global TOML document or expose executor credentials or runtime state.
### `groundhog config profiles`
List the sorted union of global and project profile names:

```text
PROFILE    ACTIVE    GLOBAL    PROJECT    NETWORK
local      yes       yes       yes        local
devnet               yes       yes        devnet
staging              yes       no         devnet
```

The command must:

- mark the active profile and the source that selected it;
  
- show whether each profile exists globally, in the project, or in both;
  
- show the effective network when one is configured;
  
- not invent profiles from `local_runtime_state`;
  
- include the built-in `default` only when it is active, and label it built-in.
  

If profile selection names a profile that does not exist, include a synthetic active row marked `missing` and exit successfully. This lets `config profiles` diagnose a stale `GROUNDHOG_PROFILE` or `default_profile` instead of failing before it can explain the problem.
### `groundhog config edit [PROFILE]`
Open a form-style terminal UI for one profile. With no profile argument, edit the active profile.

Options:

```text
--global     Edit the global layer
--project    Edit the project layer
```

If neither scope is supplied, the UI must show a scope selector before edits can be committed. It may preselect the only existing layer, but the destination path must remain visible throughout the session.

The editor covers the typed scalar fields only:

```text
Network
RPC URL
Faucet URL
Default key
Runtime manifest
Runtime artifacts directory
```

It also shows a read-only effective preview so users can see inherited values and the result of staged changes. Contract aliases and wallets may appear in a read-only section but are not edited in v1.

The selected profile must exist in the chosen target layer. If it exists only in the other layer, the UI may offer to create an empty overlay after explicit confirmation, then edit that overlay. A profile missing from both layers fails with guidance to run `config create`.

Clearing a persisted field is not supported in v1 because that is an `unset` operation with inheritance semantics. Empty inherited fields remain visibly inherited; deleting a stored value must wait for the deferred `config unset` design.

Required behavior:

- load and retain the original document before opening the UI;
  
- stage all changes in memory;
  
- validate fields as they change and validate the complete candidate before saving;
  
- show the destination file before confirmation;
  
- save the profile in one atomic commit;
  
- leave the file byte-identical on cancellation or validation failure;
  
- detect a concurrent file change at commit time and ask the user to reload rather than overwriting it;
  
- restore terminal state after success, cancellation, panic, or I/O failure;
  
- reject `--json`, `--quiet`, or non-terminal input/output immediately rather than prompting or hanging.
  

An optional "Test RPC" action may probe the staged RPC URL, but it must not be a save requirement and must not mutate configuration.
### `groundhog config set <FIELD> <VALUE>`
Set one typed field in one profile layer.

```bash
groundhog config set network devnet --profile devnet --global
groundhog config set rpc-url https://rpc.example --profile devnet --global
groundhog config set faucet-url https://faucet.example --profile devnet --global
groundhog config set default-key alice --profile devnet --project
groundhog config set runtime-manifest ./runtime.json --profile local --project
```

Supported field names:

```text
network
rpc-url
faucet-url
default-key
runtime-manifest
runtime-artifacts-dir
```

Scope rules:

- `--global` and `--project` conflict;
  
- when a project config exists, a non-interactive write requires one of them;
  
- outside a project, omitted scope means global;
  
- `--project` fails when no project config is discoverable and does not create a `groundhog.toml` in an arbitrary directory;
  
- successful output names the profile, field, and file that changed.
  

`set` parses and validates the value before acquiring the write lock, then revalidates the edited profile and document structure inside the write transaction. Unrelated profiles are preserved but do not block the repair. The profile must already exist in the target layer; `set` never creates a profile or overlay implicitly. Use `config create` first.
### `groundhog config create <PROFILE>`
Create a profile in one layer.

```bash
groundhog config create staging --global
groundhog config create staging --from devnet --project
```

The same scope rules as `config set` apply. Creating a profile that already exists in the target layer fails, even if a same-named profile exists only in the other layer.

Without `--from`, create an empty profile table. With `--from`, snapshot the source profile's durable global/project merge into the new profile. This is a copy, not persisted inheritance. Copy all six profile scalar fields plus contract aliases and wallet bindings. Do not copy executor credentials, private key data, top-level global default-key fallback, environment overrides, or `local_runtime_state`.

Copied runtime paths are written as normalized absolute paths. This preserves their meaning when the source and destination use different config directories, at the cost of replacing their original relative spelling.

The command reports exactly which layer was created. A project clone should usually remain a small overlay; the command may warn when cloning a large effective profile into project config, but its result remains deterministic.
### `groundhog config use <PROFILE>`
Set `default_profile` in one layer:

```bash
groundhog config use devnet --global
groundhog config use local --project
```

The target profile must exist in global or project configuration; `use` does not create it. The same write-scope rules as `set` apply.

If `GROUNDHOG_PROFILE` is set, the command still updates the selected file but warns that the environment override remains active. JSON output records that warning in the standard envelope.
## Provenance model
The existing `ResolvedProfile.source` is insufficient because a merged profile can have a different source for every field. Add a resolution result that pairs each exposed value with a source kind:

```text
builtIn
global
project
environment
localRuntime
cliOverride
```

The source includes a file path for global and project values and an environment variable name for environment values. Contract and wallet map entries retain individual provenance.

Normal `config get` describes the baseline resolved profile defined above. It does not invent command-specific CLI overrides or fold consumer-specific environment overrides into persisted values. The provenance model should still be reusable by `status` or future command diagnostics that need to explain a fully resolved invocation.

Relative paths must retain their defining file as their base. A global relative path resolves relative to global `config.toml`; a project relative path resolves relative to `groundhog.toml`. Human source output should make that base clear.
## Persistence and concurrency
### Document-preserving writes
Project alias and wallet writes already use `toml_edit`. Global mutations currently deserialize and serialize the full typed structure, which removes comments, formatting, and unknown fields.

Introduce one document-oriented mutation path for both files. It must preserve:

- comments and ordering where `toml_edit` supports them;
  
- unknown top-level keys and unknown profile fields;
  
- unrelated tables and values;
  
- unrelated document content while normalizing the global file to private permissions;
  
- the existing project file's permissions.
  

All existing global writers must use the same path, including `init`, `keys use`, default-key cleanup, node runtime-state updates, and credential updates. Otherwise a later command could erase formatting or unknown fields preserved by the editor.
### Locking
Atomic rename protects readers from partial files but does not prevent two read-modify-write operations from losing one another's changes.

Use a stable sidecar lock per config path. The lock must cover:

```text
lock -> fresh read -> parse -> mutate -> validate -> atomic replace -> unlock
```

All config writers must follow the same protocol. Lock acquisition has a bounded wait and returns an actionable error on contention.

The interactive editor must not hold the lock during the UI session. At commit, it acquires the lock, rereads the file, and compares it with the original. A change produces a concurrent-edit error instead of a blind overwrite.

Writes continue to use same-directory temporary files and atomic rename. Every global write normalizes the global config to mode `0600` on Unix, matching current behavior. Existing project files preserve their mode; a newly created project file uses normal non-secret file creation permissions subject to the process umask.
## Validation
Use one field validator across `get`, `edit`, `set`, `create`, `use`, and existing profile consumers. Parsing always validates TOML structure, but an operation validates only the selected profile/layer and the candidate it will write; an unrelated invalid profile must not block inspection or repair.

Validation rules include:

- profile names are trimmed before validation and storage, are non-empty, and contain no control characters;
  
- `network` is `local`, `devnet`, or `mainnet`;
  
- RPC and faucet URLs are absolute `http://` or `https://` URLs;
  
- a configured default key name is non-empty; interactive and `set` flows allow a name not present on this machine but emit a warning so portable/shared profiles remain possible;
  
- runtime paths are non-empty and retain layer-relative resolution semantics;
  
- present contract addresses and wallet addresses remain valid 32-byte `0x` hex values;
  
- a present non-empty wallet owner public key is valid 32-byte `0x` hex; legacy bindings with an omitted/empty owner key remain readable and produce a warning until rewritten by wallet management;
  
- malformed TOML errors include the file path and source location;
  
- unknown fields are preserved and ignored by typed editing, not deleted.
  

Path existence is not required at write time because a runtime manifest or artifact directory may be created later. Existing command-time checks remain authoritative when a path is consumed.

The editor may warn about suspicious combinations, such as a `devnet` profile with a local runtime manifest, but should use the same backend-selection rules as current commands rather than inventing a second policy.
## Secrets and protected state
No config command may print or edit:

- `[executor].password` or other executor credentials;
  
- private key material or key-file contents;
  
- the raw `[local_runtime_state]` record or its internal node ID;
  
- cached network descriptors;
  
- pending wallet-create or airdrop attempt records.
  

The implementation must construct allowlisted output models. It must never redact after serializing the complete global document because future secret fields could otherwise leak by default.

`local_runtime_state` still participates in baseline resolved values where relevant. The resulting non-secret RPC URL and runtime paths may be shown as profile values with source `localRuntime`, but output must not dump the record, internal node ID, or manifest content.
## Output contracts
Every non-interactive config command supports human and JSON output. JSON uses the existing `CommandResult` envelope and command names:

```text
config.get
config.profiles
config.set
config.create
config.use
```

Command data uses `schemaVersion: 1` and camelCase fields. Stable source records use explicit source kinds instead of formatted strings.

Example `config.get` data:

```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "selectionSource": { "kind": "project", "path": "/work/app/groundhog.toml" },
  "scope": "resolved",
  "values": {
    "network": "devnet",
    "rpcUrl": "https://rpc.devnet.example",
    "faucetUrl": "https://faucet.devnet.example",
    "profileDefaultKey": "alice",
    "effectiveSigningKey": "alice",
    "runtimeManifest": null,
    "runtimeArtifactsDir": null
  },
  "contracts": {},
  "wallets": {},
  "sources": null,
  "overrides": []
}
```

When requested, `sources` mirrors the value structure with records shaped as `{ "kind": "project", "path": "/work/app/groundhog.toml" }` or `{ "kind": "environment", "name": "GROUNDHOG_PROFILE" }`. A raw layer view sets `scope` to `global` or `project`, reports only stored values from that layer, and sets derived `effectiveSigningKey` to `null`.

`overrides` records recognized active consumer-specific overrides as `{ "field": "runtimeManifest", "value": "/tmp/runtime.json", "source": { "kind": "environment", "name": "GROUNDHOG_RUNTIME_MANIFEST" } }`. The current CLI has no general `GROUNDHOG_RPC_URL` override, so the config command must not report one until resolution actually supports it. `GROUNDHOG_PROFILE` appears as `selectionSource`, not in `overrides`.

`config.profiles` data contains `activeProfile`, `selectionSource`, and sorted profile rows with `global`, `project`, `builtIn`, `missing`, and effective `network` fields. Mutation results contain `schemaVersion`, `profile`, `scope`, `path`, `changed`, and `field` where applicable. The environment-selection warning uses stable code `profile_environment_override_active`; missing local keys use `profile_default_key_not_found`; legacy owner-less wallet bindings use `wallet_owner_public_key_missing`. Results never include the entire global file.
## Errors and edge cases
Add stable config-specific errors for:

- invalid profile name;
  
- profile not found or already present in the target layer;
  
- project config unavailable;
  
- ambiguous or conflicting write scope;
  
- unsupported field;
  
- invalid typed value;
  
- malformed or invalid config, including path and TOML location;
  
- lock contention;
  
- concurrent interactive edit;
  
- interactive terminal required;
  
- unsupported `config edit --json` or `--quiet`;
  
- destination write or atomic replacement failure.
  

Important behavior:

- a profile may exist globally, in the project, or in both;
  
- creating a project profile with the same name as a global profile is valid and creates an overlay;
  
- a missing built-in `default` profile may still resolve as an empty fallback, but `config use default` must not silently persist or create it;
  
- empty effective maps and unset scalar fields are normal output;
  
- project `get` outside a project is an error, not an empty document;
  
- setting an unchanged value succeeds and reports `changed: false`;
  
- environment-selected profiles remain selected until the environment changes;
  
- stale local runtime state remains owned by node lifecycle/status diagnostics, not removed by config editing;
  
- interruption before atomic rename leaves the original document intact.
  
## Implementation shape
Keep the work inside `crates/tools/cli`; no RPC or protocol changes are needed.

Expected areas:

- command parsing and dispatch in `src/main.rs`;
  
- a config command module for human/JSON orchestration;
  
- shared typed field and validation definitions under `src/config/`;
  
- document-preserving read/write transactions and locking under `src/config/`;
  
- provenance-aware resolution alongside the existing profile resolver;
  
- stable JSON report types;
  
- a terminal UI module with explicit terminal lifecycle handling;
  
- config-specific `CliError` variants and hints.
  

Avoid duplicating a second profile resolver in the command module. Effective inspection, normal command resolution, and status diagnostics should share the same merge rules.
## Phasing
### Phase 1: safe non-interactive foundation
- document-preserving global and project mutations;
  
- shared config lock used by all writers;
  
- typed validation and profile-name validation;
  
- provenance-aware resolution;
  
- `config get`, `config profiles`, `config set`, `config create`, and `config use`;
  
- stable JSON reports.
  
### Phase 2: interactive editor
- `config edit` terminal UI;
  
- effective preview and scope selection;
  
- optimistic concurrent-edit detection;
  
- terminal restoration and non-TTY behavior;
  
- optional RPC probe.
  

The TUI may land in the same user-facing release, but it should build on the same mutation and validation APIs as Phase 1 rather than owning another write path.
## Acceptance criteria
1. A user can discover the active profile and its effective values without reading TOML.
  
2. Source output identifies global, project, built-in, and local-runtime contributions per field and map entry, reports environment-based profile selection, and lists consumer-specific environment overrides separately.
  
3. A user can create, select, and update profiles globally or per project.
  
4. Non-interactive writes never prompt and clearly identify their destination.
  
5. The interactive editor cannot overwrite a concurrently changed file.
  
6. All config writes preserve unrelated comments and unknown fields.
  
7. Concurrent CLI writers do not silently lose one another's changes.
  
8. Global config remains private and no command exposes credentials, key material, or protected runtime state.
  
9. Relative runtime paths keep the same resolution semantics as hand-written TOML.
  
10. Human and JSON output are stable, deterministic, and free of mixed status text on JSON stdout.
  
## Test plan
Unit and config tests should cover:

- active-profile precedence and selection provenance;
  
- global/project scalar and per-entry map merging;
  
- effective, global-only, and project-only views;
  
- every typed field's valid and invalid forms;
  
- write-scope conflicts and project absence;
  
- create empty, clone effective, duplicate, and missing-source cases;
  
- global/project `use` and the environment-override warning;
  
- comment, ordering, unknown-field, and permission preservation;
  
- concurrent writers, lock timeout, and atomic-failure cleanup;
  
- relative path bases;
  
- omission of credentials and protected runtime state.
  

Binary integration tests should cover command parsing, human output, JSON schema, stable errors, and real file mutation under isolated XDG directories.

TUI tests should separate state transitions from terminal rendering where possible and cover save, cancel, invalid input, non-TTY rejection, terminal restoration, and concurrent-edit rejection.
## Deferred follow-ups
- `config unset` with explicit reveal-inherited semantics;
  
- profile removal and default-profile safety checks;
  
- contract alias and wallet binding editing;
  
- import/export and organization-managed profile bundles;
  
- a schema version and migration framework if incompatible config evolution becomes necessary;
  
- editor integration through `$EDITOR`;
  
- profile health checks beyond an optional RPC probe.
