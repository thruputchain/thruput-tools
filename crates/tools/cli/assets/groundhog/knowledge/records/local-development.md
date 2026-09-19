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
