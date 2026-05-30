# Crate Groups

The workspace groups crates by protocol boundary and usage:

- `json-rpc/` contains the public JSON-RPC stack: API types, jsonrpsee traits,
  shared server implementation, gateway, typed client, and OpenRPC generation.
- `x23/` contains internal x23 transport helpers used to communicate with hogd
  services. This is not the public API surface.
- `tools/` contains developer-facing tools and supporting libraries.
- `dev/` contains local development, simulation, benchmark, and process-level
  test harnesses.

Crate package names are intentionally unchanged by this layout.
