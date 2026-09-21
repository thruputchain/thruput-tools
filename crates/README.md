# Crate Groups

The workspace groups crates by protocol boundary and usage:

- `json-rpc/` contains the public JSON-RPC stack: API types, jsonrpsee traits,
  shared server implementation, gateway, typed client, and OpenRPC generation.
- `x23/` contains the internal layer that talks to hogd's x23 services:
  transport helpers and the shared backend adapters that decode hogd results
  into public JSON-RPC payloads. This is not the public API surface.
- `tools/` contains developer-facing tools and supporting libraries.
- `dev/` contains local development, simulation, benchmark, and process-level
  test harnesses.

JSON-RPC crate package names and directories use explicit `json-rpc-*` names.
OpenRPC, x23, tools, and dev crate names remain domain-specific.
