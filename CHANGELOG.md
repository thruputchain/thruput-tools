# Changelog

## Unreleased

### Contract builds

`thruput build` reads SDK 0.56's contract metadata: raw spec format 5 in the
`thruputspecrawv5` section, the `thruput_rv32_interfaces` interface table, and
`[package.metadata.thruput] raw_spec_format`. Contracts built against an older
SDK are refused with a format-skew error naming their format; rebuild them
against SDK 0.56. The SDK's linker script is now `thruput-rv32.ld`: existing
projects must change `-Tgroundhog-rv32.ld` to `-Tthruput-rv32.ld` in
`.cargo/config.toml` (the build warns, and a failed link says so). SDK 0.56 also
moves interface IDs, the ERC20 error selectors and the ERC20 event signatures
to `thruput` preimages.

### Local runtime

The default local runtime is hog-devnode 0.1.31-rc.16, the first on protocol
6705996 (protocol 0.61) and built against SDK 0.56's renamed hash domains: its
native token emits `0.1::thruput-erc20-if::*` event topics and the new ERC20
error selectors, so event filters on the old `groundhog-erc20-if` topics no
longer match. 0.1.31-rc.14 remains selectable but reports an older protocol, so
`thruput node` refuses to run it with this release.

### Breaking Rust API changes

Tools now uses protocol 0.61.0 and SDK 0.57.0. Rust consumers that exchange
protocol types with the public client or JSON-RPC crates must update their
protocol dependencies to 0.61.0 and SDK dependencies to 0.57.0; types from
the previous versions are not interchangeable. Newly scaffolded projects pin SDK 0.57.0.
The XDR codec and RPC driver now come from crates.io: depend on
`xdrs = { package = "thruput-xdrs", version = "0.17.1" }` (and
`thruput-xdr-common`, `thruput-rpc-driver` likewise) instead of the `thruput`
registry's `xdrs` 0.12, `xdr-common` 0.12 and `rpc_driver` 0.15. The APIs are
unchanged, but the crates are distinct, so their types do not mix.
These change the Rust dependency boundary, not the public JSON-RPC wire format.

The published `thruput-hogd-backend` crate now requires transaction context in
its event-parsing and transaction-lookup helpers so warnings include the hash
even when handler spans are disabled. Update Rust callers as follows:

| Previous call | Updated call |
| --- | --- |
| `event::parse_emitted_event(raw)` | `event::parse_emitted_event(raw, tx_hash)` |
| `event::parse_emitted_events(raw)` | `event::parse_emitted_events(raw, tx_hash)` |
| `indexer::tx_lookup(service, result)` | `indexer::tx_lookup(service, tx_hash, result)` |

The event helpers take `Option<TxHash>`: pass `Some(tx_hash)` for a known
transaction, or `None` for simulations without a transaction hash.
`tx_lookup` takes the requested `TxHash` as its second argument. The hash type
is `thruput_json_rpc_types::transaction::TxHash`.

The published `thruput-json-rpc-api` crate no longer defines the legacy
`AirdropApi` and `LocalFundingApi` jsonrpsee traits or their generated
`*Client`, `*Server` and `*OpenRpc` items. No binary registered them after the
wire 14 native API. Call `faucet_requestAirdrop` and `dev_fundAccount` through
`thruput_json_rpc_client::NativeClient`, and implement servers with
`thruput_json_rpc_server::native::funding::NativeFundingPlane`.

The published JSON-RPC crates no longer carry the legacy pre-wire-14 RPC
surface, which no binary served:

- `thruput-json-rpc-api` drops the `GroundhogApi` trait and its generated
  `GroundhogApiClient`, `GroundhogApiServer` and `GroundhogApiOpenRpc` items.
  Use `thruput_json_rpc_client::NativeClient` and `native::NativeApi`.
- `thruput-json-rpc-server` drops `ViewPlane`, `SubmitPlane`, `ReadPlane`,
  `BuildInfo`, `tx::validate_raw_transaction` and
  `GroundhogRpcServer::unified`. Build servers with
  `native::service::NativeService`, implement simulation with
  `native::read::NativeSimulationPlane` and the other `native::read` and
  `native::transaction` planes, and validate decoded transactions with
  `tx::validate_raw_transaction_bytes`.
- `thruput-hogd-backend` drops the legacy decoders those traits used:
  `view::{view_outcome, view_batch_response, root_output, ViewOutcome}`,
  `db_gateway::{single_item, single_item_read, storage_item_read,
  address_auth_data_read}`, `indexer::{event_page, query_error,
  transaction_ref_page, public_event_page, DecodedEventPage, DroppedEvent}`
  and `receipt::{transaction_receipt, DecodedReceipt, exec_status,
  root_output_buffers}`. The `native` module decodes the same backend results,
  for example `native::simulation`, `native::simulations` and
  `native::history::event_page`.

The remaining legacy-only items are gone too:

- `thruput-json-rpc-server`:
  - `GroundhogRpcServer<S, C, R, V>` is now
    `native::transaction::NativeTransactionService<S, C, R>`, and the public
    `server` module is gone. It serves only the transaction lifecycle:
    `new(submit, chain, read, config)` replaces the seven-argument
    constructor, `with_bundle_identity` is gone, and the methods are
    `send_transaction`, `get_transaction_status` and
    `get_transaction_receipt`. Most callers should build
    `native::service::NativeService` and call its `with_bundle_identity`
    instead.
  - `ChainPlane::get_block_by_number` is gone. Delete it from `ChainPlane`
    implementations and serve blocks with `native::read::NativeBlockPlane`.
  - The root `validate_block_transaction_page` re-export is gone. Validate
    native block pages with `native::read::validate_block_transaction_page`.
  - `ViewBatchConfig::server_limits` is gone. `getServerLimits` reports the
    batch policy as `responses::SimulationBatchLimits`.
- `thruput-hogd-backend` drops `indexer::block_lookup`. Use
  `native::block_lookup`.
- `thruput-json-rpc-types`:
  - Drops the `address_auth`, `server_limits` and `dev` modules,
    `view::{ViewBatchResponse, ViewCallResult}` and
    `transaction::BlockTransactionQuery`. The native `responses` module has
    the wire shapes, for example `responses::ServerLimits`.
  - Drops the `From` conversions between its `transaction` and `events`
    types and `thruput_indexer`, except `events::IterationOrder`.
  - `airdrop::FundAccountResponse` no longer implements `Serialize`,
    `Deserialize` or `JsonSchema`. The `dev_fundAccount` result type is
    `responses::FundingSubmission`.
- `thruput-open-rpc-macros`: `#[open_rpc]` takes no arguments and documents
  only object parameters. Remove `params = "object"`. The positional mode
  and the `namespace` and `tag` options are gone. Every method needs one
  request DTO, and the trait must not also carry jsonrpsee `#[rpc]`.
- `thruput-open-rpc` drops `param_descriptor` and `Tag`. `MethodDoc` has no
  `tags` field, and `MethodDoc::named_params` is now required. Build its
  `params` and `named_params` with `named_params`.

### Native token naming

The native token is no longer called HOG. Commands, help text and output say
"native tokens", and every HOG-named field, flag, alias and Rust item is
renamed:

- `thruput node --initial-account-hog` is now `--initial-account-tokens`.
- `thruput init` writes the native token contract alias as `NATIVE_TOKEN`
  instead of `HOG`. Existing `HOG` aliases still resolve but are no longer
  maintained; rename them in `config.toml`.
- `--json` reports rename `amountHog` to `amountTokens` (airdrop, schema 6),
  `balanceHog` to `balanceTokens` (balance schema 4, airdrop observations),
  and `fundingHog` and `minimumFundingHog` to `fundingTokens` and
  `minimumFundingTokens` (deploy, schema 9).
- The `invalid_hog_amount` error code is now `invalid_native_token_amount`.
- Network descriptors (schema 7) carry `nativeToken` instead of `nativeHog`,
  and the faucet config section `[network.native_hog]` is now
  `[network.native_token]`. Upgrade the faucet, gateway and CLI together.
- `thruput-signing` renames `NATIVE_HOG_*` to `NATIVE_TOKEN_*`, the `hog`
  module to `native_token`, `parse_hog_amount` and `ParseHogAmountError` to
  `parse_native_token_amount` and `ParseNativeTokenAmountError`,
  `native_hog_transfer_calldata` to `native_token_transfer_calldata`,
  `*DevWalletHogTransfer` to `*DevWalletNativeTokenTransfer`, and
  `build_signed_dev_wallet_hog_transfer` to
  `build_signed_dev_wallet_native_token_transfer`.
- `thruput-json-rpc-types` renames `NativeHogDescriptor` to
  `NativeTokenDescriptor` and `NetworkDescriptor::native_hog` to
  `native_token`.

The JSON-RPC wire format changes only in documentation (wire 14.1.1).

### Faucet web

The faucet web app works with the current Devnet again. It now requests
airdrops with `@thruputchain/client` through the RPC gateway's
`faucet_requestAirdrop`, instead of its own client for the faucet's HTTP API,
and its address field takes a C StrKey or base58 address. Installing it needs
a token with `read:packages` in pnpm's user-level config (see its README),
including on Railway. `VITE_GROUNDHOG_FAUCET_API_BASE` and the
`/api` proxy are gone; `VITE_THRUPUT_RPC_URL` selects a gateway other than
public Devnet. CI now checks its formatting and build.

### Gateway metrics

`hog-gateway` no longer exports
`groundhog_gateway_malformed_events_dropped_total`. Only the removed legacy
event and receipt decoding recorded it; native decoding rejects a result with
malformed events instead. Remove dashboards or alerts that read it.
