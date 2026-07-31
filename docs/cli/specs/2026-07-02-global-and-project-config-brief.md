# Global and Project Config Brief

**Date:** 2026-07-02
**Status:** Superseded by
[`2026-07-23-config-management-design.md`](2026-07-23-config-management-design.md).
This file records the original direction and is not the current behavior contract.

**Companion to:**

- [`2026-06-25-remote-devnet-faucet-design.md`](2026-06-25-remote-devnet-faucet-design.md)
  - profile-based hosted Devnet faucet and wallet-create flows.

## Goal

Make wallet, balance, faucet, and local-node CLI flows work from any directory,
without requiring a smart contract project or cwd-local `.groundhog` state.

The current project-local `groundhog.toml` model works for contract projects but
is awkward for account-level operations. Users should be able to initialize a
global Groundhog config once, then run commands such as:

```bash
groundhog wallet create --key alice --profile devnet
groundhog airdrop alice --profile devnet
groundhog balance alice --profile local
```

from any working directory.

## Proposed Config Model

Add profile support to the existing global config file:

```text
~/.config/groundhog/config.toml
```

Project-local `groundhog.toml` remains supported and overrides global config
when present. Resolution order should be:

1. CLI flags.
2. Environment variables.
3. Project-local `groundhog.toml`, if discovered by walking up from cwd.
4. Global `~/.config/groundhog/config.toml`.
5. Built-in defaults.

When `--profile` is omitted, the active profile name should resolve in this
order:

1. `GROUNDHOG_PROFILE`.
2. Project-local `default_profile`, if present.
3. Global `default_profile`, if present.
4. Built-in `default` fallback.

Project and global profiles merge field-by-field. Project values override global
values for the same profile name, while omitted project fields inherit from the
global profile. Map-like fields such as `contracts` and `wallets` also merge,
with project aliases overriding global aliases of the same name.

Profiles keep the same shape in both locations:

```toml
default_profile = "local"

[profile.devnet]
rpc_url = "https://rpc.devnet.example"
faucet_url = "https://faucet.devnet.example"

[profile.local]
rpc_url = "http://127.0.0.1:9545"
runtime_manifest = "/home/user/.local/share/groundhog/local-node/runtime-manifest.json"
```

## Init Behavior

`groundhog init` initializes or updates global CLI config by default. It should
not create a smart contract project; that remains `groundhog new`'s job.

Required behavior:

- create or update `~/.config/groundhog/config.toml`;
- set `default_profile = "local"` on first setup;
- add standard `devnet` and `local` profiles;
- preserve existing keys, contracts, wallets, and profile fields;
- make project config override global config instead of replacing it.

`groundhog init` should not write project-local config. Project-local config is
created by `groundhog new` and updated by commands that record project-owned
state.

`groundhog new` should still create a project-local `groundhog.toml`, but it
should be a minimal project overlay rather than a duplicate of global setup. It
should show project-specific contract and wallet alias sections for standard
profiles, but should not write `default_profile`, RPC URLs, faucet URLs,
runtime manifests, or default keys.

The command also needs a way to locate the local devnode binary at init time.
Possible inputs:

- explicit flag, e.g. `--hog-devnode-binary /path/to/hogd-dev`;
- current `GROUNDHOG_DEVNODE_BINARY` environment variable;
- sibling path next to the installed `groundhog` binary;
- `PATH` lookup.

The chosen path can be saved in global config if we introduce a stable config
field for local node tooling. If not saved, `groundhog node` should continue to
resolve the binary using the existing runtime lookup order.

## Local Node Runtime Manifest

Move local-node runtime discovery into profile config.

Today `groundhog node` writes `.groundhog/local-node.json` under the cwd, and
commands discover it by walking up from the current directory. That behavior was
introduced for project-local development, but it conflicts with global config and
"run from anywhere" account flows.

Future behavior should be:

- `groundhog node --profile <name>` starts the local node for that profile;
- the CLI records the node's RPC URL and runtime manifest path in the selected
  profile;
- later commands read `profile.<name>.runtime_manifest` directly;
- cwd-local `.groundhog/local-node.json` is no longer the source of truth.

This means users should not need to start `groundhog node` from a specific
project directory just so later CLI commands can find the runtime manifest.

Profile names are not special. If a user runs `groundhog node --profile devnet`,
the CLI should update `profile.devnet` with the local-node RPC URL and absolute
runtime manifest path, even though that profile name may usually refer to a
hosted Devnet profile.

When no project `groundhog.toml` is discovered, commands that persist profile
state should write to global config.

## Open Questions

- None currently. This brief intentionally keeps profile names semantic-free and
  uses merge semantics instead of separate network abstractions.
