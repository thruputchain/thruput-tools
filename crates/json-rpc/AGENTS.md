# RPC guidance

## OpenRPC Wire Version

`WIRE_FORMAT_VERSION` in [json-rpc-api/src/lib.rs](json-rpc-api/src/lib.rs)
versions the public wire contract independently of the Cargo workspace version.
Bump major for breaking method, parameter, result, or error changes; minor for
additions; patch for documentation or example refinements.

## Contract changes

Use the declarations and codecs in `json-rpc-api` and `json-rpc-types`.
For method/error changes, check the catalog and per-method mapping in
`json-rpc-types/src/error/native/`, and the generator in
`open-rpc-spec/src/native.rs`.

Do not hand-edit the generated OpenRPC snapshot. Follow
[spec_snapshot.rs](open-rpc-spec/tests/spec_snapshot.rs) to regenerate it,
inspect the diff, and rerun without `UPDATE_SNAPSHOTS`.
