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

`groundhog status` and shared result cards use responsive terminal rendering.
Use `--no-ui` for plain human output without boxes or terminal controls. It keeps
existing confirmation rules; use `--json` for machine-readable command results.

On compatible foreground terminals, `groundhog build` shows delayed phase and
elapsed-time progress, then an artifact summary with compiler diagnostics and
Groundhog warnings below it. Compiler messages retain source locations, snippets,
and notes; identical diagnostics from the same package appear once. Use
`groundhog build --verbose` (or `-v`) to see Cargo logs live. Resizing or resuming a
stopped job switches progress to append-only output to preserve scrollback.
Redirected streams, `--no-ui`, and explicit Cargo output-format/color settings
keep live append-only logs. `--json` retains its machine-readable output contract.

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
