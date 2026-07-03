# Global and Project Config Brief

**Date:** 2026-07-02
**Status:** Follow-up design brief. Intended to guide a separate PR after the
hosted Devnet faucet work lands.

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

Add a global config file under `~/.config/groundhog`, for example:

```text
~/.config/groundhog/groundhog.toml
```

Project-local `groundhog.toml` remains supported and overrides global config
when present. Resolution order should be:

1. CLI flags.
2. Environment variables.
3. Project-local `groundhog.toml`, if discovered by walking up from cwd.
4. Global `~/.config/groundhog/groundhog.toml`.
5. Built-in defaults.

Profiles keep the same shape in both locations:

```toml
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

- create `~/.config/groundhog/groundhog.toml` when requested;
- add standard `devnet` and `local` profiles;
- preserve existing keys, contracts, wallets, and profile fields;
- optionally write project-local config when run in project mode;
- make project config override global config instead of replacing it.

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

## Open Questions

- What flag or subcommand should `groundhog init` use when a user explicitly
  wants to write project-local config?
- Should project config inherit global saved wallets by default, or should wallet
  bindings be profile-local and fully overridden by project config?
