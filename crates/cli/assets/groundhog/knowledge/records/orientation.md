+++
schema_version = 1
id = "groundhog.orientation"
owner = "groundhog-rpc"
kind = "orientation"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 10
+++
# Groundhog

Groundhog is not EVM-compatible. Do not infer its execution, storage, account,
transaction, gas, or ABI behaviour from Ethereum, Solana, or another chain.
Use the installed CLI and the project's public Groundhog dependencies as the
authority for this release.

## Start Here

Before editing a Groundhog project:

1. Read the project's `AGENTS.md` and `groundhog.toml` when present.
2. Run `groundhog --version` and the relevant command's `--help`.
3. Run `groundhog status --json` when environment, toolchain, project, profile,
   key, or local-node state matters.
4. Identify which Groundhog layer owns the task before reading source.
5. Prefer public APIs, generated bindings, and current examples. Inspect
   `node_modules`, Cargo registry source, or Groundhog implementation
   repositories only after those public surfaces are insufficient.

Do not guess an installed command's syntax from this skill. The installed
binary's `--help` output wins when it differs.

## Know the Layers

| Layer | Owns |
|---|---|
| Rust contract | Methods, events, exceptions, authorization, and persistent state |
| Rust SDK | Contract macros, storage types, generated contract clients, and native testing |
| Contract build | The deployable `.rv32a` artifact and canonical `.spec.xdr` interface |
| Generated TypeScript binding | Typed method and event descriptors derived from `.spec.xdr` |
| TypeScript client | RPC transport, clusters, accounts, wallets, transactions, receipts, and events |
| Wallet | The authorized root invocation around one or more application calls |
| Groundhog CLI | Scaffolding, builds, tests, local nodes, keys, deployment, calls, inspection, and codegen |
| JSON-RPC | The public network boundary used by the CLI and TypeScript client |

Keep these boundaries explicit. In particular:

- A generated TypeScript binding describes a contract. It is not a connected
  contract instance and its methods are not direct RPC functions.
- The application invocation is normally a child of a wallet root invocation.
- The CLI and TypeScript client consume the public RPC API. Do not bypass them
  by constructing internal protocol messages unless the task specifically
  requires protocol work.
