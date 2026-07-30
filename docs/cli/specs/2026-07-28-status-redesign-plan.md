# Groundhog Status Redesign Plan
**Date:** 2026-07-28  
**Status:** Ready to execute  
**Workstream:** Config management  
**Depends on:** Persisted profile read model and provenance from config-management Slice 3
## Goal
Make `groundhog status` a concise view of the configuration Groundhog is actively using:

- the active project, when the command runs inside one;
  
- the active profile and its effective fields;
  
- the effective active signing key;
  
- the effective active wallet.
  

The command remains read-only and works inside or outside a project. It never prints credentials, private key material, or complete config documents.
## Ground Truth
The redesign must reflect the current CLI model rather than introduce a second interpretation.
### Active Project
A project exists only when Groundhog finds `groundhog.toml` by walking from the current directory toward the filesystem root.

- Inside a project, show the project root and config path.
  
- Outside a project, omit the Project section entirely.
  
- Never print `Project: none` or treat a missing project overlay as a finding.
  

Project config is an optional overlay on global profile configuration.
### Active Profile
The profile name follows the existing precedence:

1. explicit `status --profile`;
  
2. `GROUNDHOG_PROFILE`;
  
3. project `default_profile`;
  
4. global `default_profile`.
  

Config-management Slice 3 removes the legacy `active_profile_name` fallback to `default`. If no source selects a profile, profile resolution returns a stable no-active-profile configuration error. If a source selects a name that does not exist in either config layer, resolution returns `ProfileNotFound`.

The name `default` has no special behavior. It is valid only when selected through the normal precedence and defined by a global or project `profile.default` table.

Commands that require a resolved profile propagate these errors normally. `config profiles` remains an inspection exception: it can list an active missing profile and unrelated invalid profiles. `status` converts resolution failures into warning-level `StatusReportV1` findings. No selection produces `profile: null`; a selected missing or invalid profile retains its name and selection source with status `missing` or `invalid`. Unexpected I/O and internal errors still use the generic error path.

For a configured profile, merge fields exactly as `resolve_profile` does:

- project scalar values override global values when present;
  
- project contract and wallet entries override matching global entries;
  
- unrelated global entries remain available.
  
### Local Node State
`groundhog node` writes transient global `local_runtime_state` containing:

- RPC URL;
  
- runtime manifest path;
  
- runtime artifact directory when available;
  
- an internal node-state identifier.
  

The node removes this state when it terminates normally. Status must not turn it into a separate runtime dashboard.

Apply and display this state only through the existing profile resolver:

- the active profile must have `network = "local"`;
  
- local node values fill only missing profile RPC, manifest, and artifact fields;
  
- explicit global or project profile values continue to win;
  
- non-local profiles ignore local node state.
  

When an effective field came from `local_runtime_state`, label that field `local node`. Do not show raw local node state for a non-local active profile. Do not add process inspection, RPC health probes, backend-plane output, producer metadata, or persisted binary paths.
### Active Key
Use `resolve_default_key_for_profile` as the authority.

The effective key precedence remains:

1. merged profile `default_key`;
  
2. top-level global default key selected by `groundhog keys use`;
  
3. manifest-backed `dev-0` only when the active local profile matches the transient local node RPC and manifest;
  
4. no active key.
  

Status shows the key name, source, kind (`stored` or `dev`), loadability, and public address when available.
### Active Wallet
Status reports the wallet selected for the effective key when no command-specific `--wallet` or `--from` override is supplied.

Use the same key-specific branches as transaction commands:

- For a dev key, reject a configured profile default wallet; otherwise load the fixed dev wallet from the effective runtime manifest.
  
- For a stored key, use the merged profile default wallet when present; otherwise use the unique saved wallet owned by that key.
  
- With no effective key, report no active wallet without a second wallet warning.
  
- Report explicit missing, ambiguous, invalid, or owner-mismatch results from those branches. Automatic owner matching ignores malformed unrelated wallet metadata, matching transaction behavior; malformed metadata is reported directly only when the configured default wallet selects that entry.
  

Dev-wallet resolution includes the existing `GROUNDHOG_RUNTIME_MANIFEST` override. When that environment value supplies the wallet manifest, show it as the active wallet source even when the profile's displayed runtime-manifest field came from config or local node state.

Extract a safe diagnostic result from `account_resolution` so status does not copy this precedence into `checks.rs`.
## Why the Current Command Fails
The current command mixes old setup checks with profile status.

- `Project: none` implies an error outside a project even though global profiles are valid there.
  
- Profile output omits several standard fields and does not explain local-node overlays.
  
- Key resolution approximates `dev-0` eligibility instead of using the canonical resolver.
  
- Wallet output shows configured defaults but not the actual active wallet selection.
  
- Cargo, Rust target, `hog-devnode`, dev-wallet artifact, registry, and executor-auth checks obscure the active configuration.
  
- Missing optional setup tools produce warnings unrelated to the selected profile.
  
- Warnings and hints appear as detached, unwrapped lines below the status box.
  
- JSON is ad hoc, unversioned, snake_case, and does not model the active key or wallet.
  
## Command Contract
Keep the current surface:

```text
groundhog status [--profile <PROFILE>]
```

Do not add RPC, manifest, wallet, project, or setup flags. Status describes the active configuration after existing profile selection and merge rules.
### Exit Codes and Streams
- Exit `0` when the active configuration has no warning-level finding.
  
- Exit `1` when the selected profile, key, or wallet is invalid or unusable.
  
- Informational findings do not change the exit code.
  
- Human output remains on stderr.
  
- `--quiet` emits no human output and preserves exit semantics.
  
- `--json` emits one `CommandResult` envelope on stdout.
  
- With both global flags, `--json` takes precedence over `--quiet`, matching current CLI behavior.
  
## Human Output
Render small sections instead of one environment/project table.
### Outside a Project
```text
Groundhog Status

Profile
  Name               local (global default)
  Network            local
  RPC URL            http://127.0.0.1:9545 (local node)
  Faucet URL         none
  Runtime manifest   /tmp/.../runtime-manifest.json (local node)
  Runtime artifacts  /tmp/.../hog-devnode (local node)
  Default key        none
  Default wallet     none

Active Identity
  Key                dev-0 (local node, dev)
  Key address        0x...
  Wallet             0x... (manifest dev account)
```

There is no Project section because the command is outside a project.
### Inside a Project
```text
Groundhog Status

Project
  Root               /work/counter
  Config             /work/counter/groundhog.toml

Profile
  Name               devnet (project default)
  Network            devnet
  RPC URL            https://rpc.example (global)
  Faucet URL         https://faucet.example (global)
  Runtime manifest   none
  Runtime artifacts  none
  Default key        alice (project)
  Default wallet     trading (project)

Active Identity
  Key                alice (profile, stored)
  Key address        0x...
  Wallet             trading · 0x... (profile default)
```
### Missing Active Profile
When no source selects a profile, do not invent or render a Profile section:

```text
Needs attention
  No active profile is configured.
    Fix: select a profile with `groundhog config use <PROFILE>` or pass --profile.
```

A selected missing profile uses the same structure with its name and the source that selected it.
## Findings
Findings cover only the active project/profile/identity path.

| Condition | Severity |
| --- | --- |
| No source selects an active profile | Warning |
| Profile selected by explicit flag, environment, project default, or global default is missing | Warning |
| Active profile has malformed fields | Warning |
| Active profile has no RPC URL | Info |
| Configured active profile has no effective key | Warning |
| Active profile references a missing or invalid key | Warning |
| Active wallet is missing, ambiguous, malformed, or owned by another key while a key is active | Warning |
| No wallet is selected because no key is active | No separate finding |
| Local profile has no current local node state and no explicit target fields | Info |
| No project overlay | No finding |
| No stored keys when a valid dev key is active | No finding |
| Cargo, toolchain, devnode binary, registry, or credentials are absent | Not checked by status |

Render findings in one bounded section. Keep each message and remedy together.

```text
Needs attention
  Wallet `trading` is not owned by active key `alice`.
    Fix: select a wallet owned by `alice` or change the profile default key.
```

- Do not prefix human messages with internal finding codes.
  
- Keep stable codes in JSON.
  
- Wrap long messages, paths, and remedies to the terminal width.
  
- Add no TUI dependency.
  
## JSON Contract
Replace the legacy payload with typed `Serialize`, `Deserialize`, and `JsonSchema` models using camelCase and `schemaVersion: 1`.

```json
{
  "ok": true,
  "command": "status",
  "warnings": [],
  "data": {
    "schemaVersion": 1,
    "project": null,
    "profile": {
      "name": "local",
      "status": "configured",
      "selectedBy": {
        "kind": "global",
        "path": "/home/user/.config/groundhog/config.toml"
      },
      "network": "local",
      "rpcUrl": "http://127.0.0.1:9545",
      "faucetUrl": null,
      "runtimeManifest": "/tmp/runtime-manifest.json",
      "runtimeArtifactsDir": "/tmp/hog-devnode",
      "defaultKey": null,
      "defaultWallet": null,
      "sources": {
        "rpcUrl": { "kind": "localNode" },
        "runtimeManifest": { "kind": "localNode" },
        "runtimeArtifactsDir": { "kind": "localNode" }
      }
    },
    "activeKey": {
      "name": "dev-0",
      "source": { "kind": "localNode" },
      "kind": "dev",
      "status": "available",
      "address": "0x..."
    },
    "activeWallet": {
      "name": null,
      "source": { "kind": "manifestDevAccount" },
      "manifestSource": { "kind": "localNode" },
      "status": "available",
      "address": "0x..."
    }
  }
}
```

Contract rules:

- `project` is null in JSON when no project exists and omitted entirely from human output.
  
- `profile` is null only when no source selects a profile; profile-resolution errors appear in envelope findings.
  
- `activeKey` and `activeWallet` are null when no profile resolves.
  
- A present `profile.status` is `configured`, `missing`, or `invalid`.
  
- Standard profile values are explicit null when unset.
  
- Reuse the config provenance source object and add `localNode`, `globalDefaultKey`, `uniqueOwnerMatch`, and `manifestDevAccount` kinds required by effective status.
  
- Active key status is `available`, `missing`, `invalid`, or `notSelected`.
  
- Active wallet status is `available`, `missing`, `ambiguous`, `invalid`, or `notSelected`.
  
- Manifest-backed wallets include `manifestSource`, which identifies profile, environment, or local-node manifest precedence; stored-wallet diagnostics set it to null.
  
- The envelope `warnings` contains informational and warning findings with `level`, `code`, `message`, and optional `hint`; only warning-level findings affect exit status.
  
- No credential state, private key data, full key files, runtime seed, or raw global config appears.
  

The old status JSON has no schema version and reflects obsolete behavior. Replace it deliberately and lock the new shape with integration tests rather than carrying a permanent compatibility projection.
## Implementation Shape
### Data Ownership
- Config-management read models own profile selection, merge provenance, and layer status.
  
- `resolve_profile` remains the authority for effective profile values and local-node overlays.
  
- `resolve_default_key_for_profile` remains the authority for active key selection.
  
- `account_resolution` remains the authority for active wallet selection.
  
- `checks.rs` retains setup checks used by `init`; it no longer defines the status report.
  
- `status.rs` composes safe projections from those authorities and renders them.
  
### Required Refactors
1. Consume strict active-profile selection and missing-profile errors from config-management Slice 3.
  
2. Preserve the config resolver's rule that `default` has no special behavior.
  
3. Consume persisted profile selection and field provenance from config-management Slice 3.
  
4. Extend effective profile diagnostics with `localNode` provenance only for fields actually filled from transient state.
  
5. Add a safe diagnostic API for effective key source, kind, availability, and address.
  
6. Extract a non-secret active-wallet diagnostic from canonical account resolution, including environment runtime-manifest precedence.
  
7. Define `StatusReportV1`, `ProjectStatus`, `ProfileStatus`, `ActiveKeyStatus`, and `ActiveWalletStatus`.
  
8. Split setup reporting used by `init --json` from the new status report before replacing status JSON.
  
9. Replace the status renderer with conditional Project, Profile, Active Identity, and Needs Attention sections.
  
10. Add bounded wrapping for diagnostic message/remedy pairs to `Print`.
  
### `init --json`
`init --json` currently aliases the legacy status report. Before status JSON changes, replace that alias with a typed `init.setup` report containing the existing environment, key, registry, and credential setup checks. It uses `schemaVersion: 1`, the shared envelope, current warning-level exit behavior, and no live status/profile fields. Human interactive `init` continues to consume the same setup checks.

Init readiness remains profile-aware when an active profile resolves, then falls back to the top-level global default key exactly as `usable_default_key_for_init` does today. A no-active-profile finding does not abort first-run setup. Human and JSON init use the same setup report and readiness rules.
## Scope Boundaries
This work does not:

- change profile merge precedence after strict active-profile selection succeeds;
  
- automatically select a local profile because `groundhog node` is running;
  
- inspect or print local node state for a non-local active profile;
  
- add RPC health checks or network calls;
  
- report backend planes, chain tip, producer metadata, or node process identity;
  
- persist the devnode binary path or additional node startup state;
  
- check Cargo, Rust targets, `hog-devnode` discovery, registry setup, or executor credentials;
  
- predict command-specific `--wallet` or `--from` overrides;
  
- replace `config get`, `config profiles`, `keys ls`, or `wallet ls` as inventory commands;
  
- add a TUI dependency.
  
## Testing
### Profile and Project
- Outside a project omits the human Project section and emits JSON `project: null`.
  
- Inside a project shows root/config paths and project selection source.
  
- Explicit, environment, project-default, and global-default selection.
  
- Resolver unit tests keep the stable no-active-profile error when no source selects a name.
  
- Resolver unit tests keep `ProfileNotFound` when the selected profile is absent from both layers, including the name `default`.
  
- Global profile fields with project overrides and per-field sources.
  
- Missing or malformed selected profile findings.

- Status command tests convert no selection and missing or invalid profiles into warning findings, the documented nullable/status fields, and exit code `1`.
  
### Local Node Overlay
- Active local profile receives missing RPC, manifest, and artifact values from transient node state.
  
- Explicit profile fields continue to override transient node values.
  
- Source labels identify only fields actually supplied by the local node.
  
- Non-local profiles do not expose or consume transient local node state.
  
- Node state removed on normal termination disappears from subsequent status output.
  
### Active Identity
- Profile key, global default key, strict manifest `dev-0`, and no-key cases.
  
- Stored key availability and safe public address.
  
- Profile-default, unique-owner, and manifest-dev wallet selection.
  
- Dev-key rejection of configured default wallets.
  
- `GROUNDHOG_RUNTIME_MANIFEST` precedence for manifest dev wallets.
  
- Missing, ambiguous, and owner-mismatch wallet findings, plus malformed metadata when the configured default selects that wallet.
  
- No command-specific wallet override is inferred.
  
### Output
- Human section order and conditional Project section.
  
- Wrapped finding/remedy formatting in TTY and non-TTY output.
  
- Quiet warning and success exits.
  
- Combined `--json --quiet` JSON precedence.
  
- Exact V1 JSON shape, camelCase names, and nullable fields.
  
- Secret omission.
  
- `init --json` uses its setup report and no longer depends on status JSON.
  
## Delivery Sequence
Execute this work after config-management Phase 1 Slice 7 and before Phase 1.6 terminal box layout hardening.
### Status Slice A: Active Profile Report
- Consume selection and field provenance from the config read model.
  
- Consume strict selection and missing-profile errors from the config read model.
  
- Add effective `localNode` provenance without changing persisted config inspection.
  
- Define typed project/profile status models.
  
- Separate `init --json` setup output from status output.
  
- Keep existing human rendering until the model and JSON contract are approved.
  
- Suggested commit: `refactor(cli): build status from active profile state`
  

Pause for profile semantics and JSON review.
### Status Slice B: Active Identity
- Add canonical key diagnostics.
  
- Extract and add canonical wallet diagnostics.
  
- Migrate `checks.rs` consumers to the shared active-key diagnostic before removing duplicate key/default-wallet approximations; retain init's top-level global-key fallback.
  
- Suggested commit: `feat(cli): report active key and wallet status`
  

Pause for stored-key, saved-wallet, and local dev-key testing.
### Status Slice C: Human Output
- Render conditional Project, Profile, Active Identity, and Needs Attention sections.
  
- Remove setup/toolchain/devnode rows and unrelated findings.
  
- Add bounded diagnostic wrapping.
  
- Suggested commit: `feat(cli): redesign status output`
  

Pause for visual review inside/outside projects and at narrow/wide terminal widths.
### Status Slice D: Documentation and Regression Gate
- Update command help, README, roadmap, JSON examples, and completion snapshots if needed.
  
- Run complete CLI and workspace quality gates.
  
- Suggested commit: `docs(cli): document active configuration status`
  
## Completion Criteria
- Outside a project, status shows no Project section.
  
- No profile is invented when active-profile selection fails.
  
- The name `default` receives no special resolution behavior.
  
- Effective profile fields match `resolve_profile`, including transient local-node overlays only for local profiles.
  
- Active key matches `resolve_default_key_for_profile`.
  
- Active wallet matches canonical sender resolution without a command-specific override.
  
- Human output contains only active project/profile/identity state and attached findings.
  
- JSON is typed, camelCase, versioned, and free of secrets.
  
- Setup checks remain available to `init` but no longer dominate `status`.
