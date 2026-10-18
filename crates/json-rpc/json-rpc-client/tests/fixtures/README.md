These are third-party meta-schemas, not RPC schemas. Tests generate the RPC
contract from Rust types and validate it offline against these fixtures.

- `openrpc-meta-1.14.9.json`: unmodified asset from
  https://github.com/open-rpc/meta-schema/releases/tag/1.14.9
  (commit ee339fd2c792f4f21ca64434c973842dc0395699). It accepts OpenRPC 1.2.6,
  which the current generator emits. License: `openrpc-meta-LICENSE`.
- `json-schema-tools-meta.json`: referenced vocabulary from
  https://meta.json-schema.tools/, retrieved 18 October 2026. Kept locally
  so the test does not resolve schemas over the network. Only trailing blank
  lines were removed. License:
  `json-schema-tools-LICENSE`.
