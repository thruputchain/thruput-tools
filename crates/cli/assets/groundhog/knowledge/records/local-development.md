+++
schema_version = 1
id = "groundhog.local-development"
owner = "groundhog-rpc"
kind = "workflow"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 60
+++
## Local Development

Use `groundhog init --non-interactive --json` for scripted setup. Missing Cargo,
profile, or stored key exits 1. Key creation requires authorized `--generate-key`;
existing selections are preserved. `--quiet` does not grant consent. `init` writes;
`status --json` is read-only. Create projects with `groundhog new`.

Start the local stack with `groundhog node`. Inspect, prefetch, or locate its
catalog-pinned bundle with `toolchain list`, `toolchain install`, and `toolchain
which`. `--offline node` forbids managed downloads, not RPC/Cargo elsewhere.
`GROUNDHOG_DEVNODE_BINARY` selects a matching build. Diagnose compatibility
failures; do not override protocols. Use `status --json` / `node --json` for runtime
information and `--help` for flags. Never use deterministic local keys in production.

`--non-interactive` disables prompts/UI without granting consent: protected
operations require authorized `--force`. Key imports need piped stdin; Cargo/test
subprocesses cannot read terminal stdin. Use `--json` for machine results.

Timeout or `unknown` does not prove transaction failure. Inspect the hash before
retrying. `node --verbose` enables backend logs; restarting an ephemeral node
loses its state.

`events list`, `tx list`, and `token transfers` read newest-first pages.
Omit `--cursor` to refresh from the head; a continuation goes toward older rows.
Continue with the returned opaque cursor and the same target/filters. `tx list`
matches root invocation targets, not every address touched by a transaction.
See [Event pages and restart](#event-pages-and-restart) for cursor rules.

### Key and address interoperability

`groundhog keys import <name>` reads an Ed25519 seed from a hidden terminal
prompt or piped stdin. It accepts a checksummed Stellar S StrKey or a 32-byte
hex seed, never a 64-byte stored keypair. Do not put private seeds in command
arguments. Existing names are not overwritten; `dev-N` names are reserved.
Import JSON uses a versioned camelCase report (`data.schemaVersion = 1`).
Malformed TOML or an invalid `default_key` is rejected before storing the key.
If the key is stored but selecting the default fails, import succeeds with a
warning and a recovery
command (`groundhog keys use <name>`); do not import it again. `isDefault` is
null when the configuration cannot be reread to confirm selection.

`groundhog keys export <name> --format strkey` exports the same seed as an S
StrKey after the normal confirmation gate. The default export remains hex.
For an authorized non-interactive transfer between key stores, pipe export
stdout directly to the receiving CLI's stdin rather than capturing it in logs.

A G StrKey is an Ed25519 public key, not a derived Groundhog account or wallet
address. Key commands display public keys separately from account addresses.
C StrKeys and legacy hex are accepted for contract/wallet address inputs,
including typed contract arguments. Saved wallet owners accept G StrKeys or
hex public keys. These encodings preserve bytes and do not identify a network.
Existing key files and public JSON-RPC wire encodings remain unchanged.

### Native accounts

Ordinary keys derive nonce-zero native accounts. `wallet create` saves the empty
identity offline; fund explicitly with `airdrop`. `--prefund` is rejected. Saved
wallets and dev-N retain identity and format.

Native deploy requires `--fund <HOG>` and hogd 0.1.29-rc.2 or later. The exact
amount is not an estimate; zero skips transfer. Add `--init <METHOD>` and repeated
`--init-arg <VALUE>` using the artifact spec, or a 0x selector and raw hex argument.
`--init-gas-limit` sets child gas. Init runs as the wallet, discards its return,
and rolls back with funding/deployment on failure. Dry-run submits nothing.
Unverified results retain signed identity: inspect before retrying because nonce
and address change. Aliases require verified success at the predicted address.
