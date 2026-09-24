# groundhog-client

Reusable contract invocation and spec codec for native Groundhog clients.
Groundhog CLI and the experimental Stellar adapter supply their own configuration,
identity resolution, confirmation, runtime and output.

## Invocation phases

1. Resolve the trusted network hash, wallet address and `doAction` entrypoint,
   contract address/entrypoint, calldata, gas limits, expiry height and nonce.
2. Use `validate_network` to compare the endpoint with the trusted network identity.
   `resolve_block_expiry_height` can derive expiry from the current chain tip.
3. `prepare_call(CallRequest, NetworkHash)` returns `UnsignedCall` for inspection.
4. `UnsignedCall::sign(&impl Signer)` returns `SignedCall`; inspect `tx_hex` and
   `tx_hash` before submission if needed.
5. `SignedCall::submit(&client)` returns `SubmittedCall`, rejecting a server hash
   that differs from the locally computed hash. Errors retain the original hash.
6. `SubmittedCall::wait(&client, PollOptions)` returns a typed receipt, including
   unsuccessful execution. The caller interprets execution status and formats it.

These phases use the existing dev-wallet `doAction` transaction/signing protocol.
They do not support arbitrary wallet protocols. Network validation is explicit;
`prepare_call` and `sign` do not contact the network. Submission is not retried
by this library. After an ambiguous submission error or receipt timeout, reconcile
by the retained hash before deciding what to do next.

The `codec` module decodes canonical specs, resolves methods, derives selectors,
encodes positional arguments and decodes returns/declared aborts. The caller
loads any files and supplies the metadata. Best-effort return decoding can return
`None`; the receipt still contains raw data. Ambient SDK exception-registry lookup
stays in the Groundhog CLI compatibility layer.

The crate owns no runtime, CLI parser, profiles, key store, project discovery,
terminal output or process lifecycle. Its normal dependencies exclude the
Groundhog CLI, node supervisor, compiler/lowerer, toolchain manager, JSON-RPC
server crate and contract SDK. Existing RPC, signing, XDR and spec types are
re-exported so adapters can use matching versions.

Node management remains a separate capability based on `groundhog-node-supervisor`.
Adding a node adapter to another CLI should not expand this crate's dependencies.

## Text encodings for host applications

`groundhog_client::strkey` re-exports the encoding helpers from
`groundhog-signing`. Use `parse_contract_address` / `format_contract_address`
for C StrKeys, `parse_public_key` / `format_public_key` for G public keys, and
`parse_secret_seed` / `format_secret_seed` for explicit S seed import/export.
Parsing also accepts legacy 32-byte hex. Invalid checksum, length or type is
rejected; errors never include the supplied input. Secret results use
`Zeroizing`; applications remain responsible for their own input buffers.

These helpers preserve bytes. G public keys do not become Groundhog account or
wallet addresses, and none of the encodings identify a network. The spec codec
accepts C addresses, including nested typed arguments. Its decoded output and
public RPC JSON remain hex for compatibility. A host that wants C output can
call the formatter explicitly. Neither CLI needs its own StrKey conversion.
