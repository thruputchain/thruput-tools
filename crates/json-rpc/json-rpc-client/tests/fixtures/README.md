These are third-party meta-schemas, not RPC schemas. Tests generate the RPC
contract from Rust types and validate it offline against these fixtures.

Both schemas use Apache 2.0. Their identical upstream license texts are retained
in [LICENSE-APACHE-2.0](LICENSE-APACHE-2.0).

- `openrpc-meta-1.14.9.json`: unmodified asset from
  https://github.com/open-rpc/meta-schema/releases/tag/1.14.9
  (commit ee339fd2c792f4f21ca64434c973842dc0395699). It accepts OpenRPC 1.2.6,
  which the current generator emits.
- `json-schema-tools-meta.json`: referenced vocabulary from
  https://meta.json-schema.tools/, retrieved 18 October 2026. Kept locally
  so the test does not resolve schemas over the network. Only trailing blank
  lines were removed.
