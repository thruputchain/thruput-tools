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

For first-run setup in automation, use `groundhog init --non-interactive --json`.
It initializes global profiles and reports incomplete setup with exit 1 when
Cargo, the active profile, or a selected stored signing key is unavailable.
Add `--generate-key` only when authorized to create the initial signing key;
existing keys and explicit key selections are preserved. `--quiet` does not
accept wizard defaults. `init --json` performs setup; use `status --json` for
read-only diagnostics. Create scripted projects separately with `groundhog new`.

Use the CLI to start the supported local stack:

```console
groundhog node
```

The CLI can acquire a catalog-pinned devnode bundle on startup. Use
`groundhog toolchain list` to inspect the embedded catalog, `toolchain install`
to prefetch it, and `toolchain which` to find the installed executable.
`groundhog --offline node` forbids managed downloads; it does not disable RPC
or Cargo network traffic in other commands. Keep `GROUNDHOG_DEVNODE_BINARY`
for a matching local build. Diagnose startup compatibility failures rather
than adding a protocol override.

Use another terminal for status, build, deploy, call, view, or TypeScript
application work. Prefer the active local profile and runtime information
reported by `groundhog status --json` and `groundhog node --json`. Do not copy
deterministic local-development keys into production configuration.

Typical CLI discovery:

```console
groundhog status --json
groundhog build --help
groundhog test --help
groundhog node --help
groundhog deploy --help
groundhog call --help
groundhog view --help
groundhog codegen ts --help
```

Use global `--non-interactive` to disable CLI prompts and terminal UI while
retaining ordinary output. It does not grant consent: protected actions still
require `--force`, and initial key creation requires `init --generate-key`.
`keys import` needs piped stdin when prompts are disabled; never place seeds
in arguments. Cargo/test subprocesses cannot read terminal stdin in this mode.

Use `--json` for agent-consumed command results. Respect commands that require
explicit confirmation, and never add a confirmation override unless the user
has authorized the consequential action.

A receipt timeout does not establish that a transaction failed. Inspect the
transaction using the command printed in the error before submitting the
operation again. An `unknown` status does not explain why the transaction is
absent. For detailed local backend logs, start `groundhog node --verbose`;
output goes to that terminal unless redirected. Do not restart an ephemeral
node just to enable logging: stopping it discards its chain state.

### Key and address interoperability

`groundhog keys import <name>` reads an Ed25519 seed from a hidden terminal
prompt or piped stdin. It accepts a checksummed Stellar S StrKey or a 32-byte
hex seed, never a 64-byte stored keypair. Do not put private seeds in command
arguments. Existing names are not overwritten; `dev-N` names are reserved.
Import JSON uses a versioned camelCase report (`data.schemaVersion = 2`).
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
CLI output, `--json` payloads, and JSON-RPC responses render contract, wallet,
and account addresses as C StrKeys and public keys as G StrKeys; key reports
also keep the hex public key. Inputs accept those StrKeys or legacy hex,
including typed contract arguments and saved wallet owners. These encodings
preserve bytes and do not identify a network. Key files stay hex, and
`groundhog test --integration` still passes `GROUNDHOG_CONTRACT_ADDRESS` as
`0x`-prefixed hex.
