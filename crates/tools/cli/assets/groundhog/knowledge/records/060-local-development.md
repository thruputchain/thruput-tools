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
