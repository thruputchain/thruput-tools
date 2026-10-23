# Changelog

## Unreleased

### Breaking Rust API changes

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
  `GroundhogRpcServer::unified`.
  `GroundhogRpcServer` now takes submit, chain and read planes only:
  `GroundhogRpcServer::new(submit, chain, read, config)`. Build servers with
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

### Gateway metrics

`hog-gateway` no longer exports
`groundhog_gateway_malformed_events_dropped_total`. Only the removed legacy
event and receipt decoding recorded it; native decoding rejects a result with
malformed events instead. Remove dashboards or alerts that read it.
