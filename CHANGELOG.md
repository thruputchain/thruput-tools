# Changelog

## Unreleased

### Breaking Rust API changes

Tools now uses protocol 0.57.0 and SDK 0.54.0. Rust consumers that exchange
protocol types with the public client or JSON-RPC crates must update their
protocol dependencies to 0.57.0 and SDK dependencies to 0.54.0; types from
the previous versions are not interchangeable. Newly scaffolded projects pin SDK 0.54.0.
The default local runtime is hog-devnode 0.1.31-rc.14. This changes the Rust
dependency boundary, not the public JSON-RPC wire format.

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

### Gateway metrics

`hog-gateway` no longer exports
`groundhog_gateway_malformed_events_dropped_total`. Only the removed legacy
event and receipt decoding recorded it; native decoding rejects a result with
malformed events instead. Remove dashboards or alerts that read it.
