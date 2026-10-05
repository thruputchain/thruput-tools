# Crate layout

Crates are grouped by subsystem. Other crates live directly under `crates/`,
using their existing names.

- `json-rpc/` contains the public JSON-RPC stack: API types, traits, shared
  server, gateway, typed client, and OpenRPC generation.
- `rv32/` contains the contract artifact format and ELF lowering.
- `x23/` contains the internal hogd integration layer: transport helpers and
  shared backend adapters that map hogd responses to public RPC payloads.

Top-level crates include the CLI, embeddable client, signing, toolchain
management, local node supervisor, services, and test tools. Libraries can
serve several of these consumers; their location does not restrict their use.

Keep existing crate names within groups. In particular, `json-rpc-client`
identifies the wire client, while `client` provides contract invocation.
Directory moves do not change Cargo package names.

See the [architecture guide](../docs/architecture.md) for component relationships
and [Cargo.toml](../Cargo.toml) for the complete crate list.
