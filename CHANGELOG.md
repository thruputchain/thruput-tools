# Changelog

## 0.45.1

### RV32 artifact format v7 declares a syscall API version instead of a gas schedule

The CLI now writes RV32 artifact format v7. A v7 header carries the syscall API
version the guest was built against, in place of the gas table version and gas
schedule digest of v5 and v6. A chain admits the artifact when it implements the
same major version and at least that minor; gas prices are the chain's and are
no longer stamped into the artifact. The v7 header is 120 bytes: the 8-byte
digest is gone and every later field sits 8 bytes earlier than in v5 and v6.

- `thruput build` and `rv32-lower` read the version from the guest ELF's
  `thruputsyscallapiv0` section, which `thruput-sdk` 0.61.1 and later emit.
  An ELF without it is refused: rebuild the contract with SDK 0.61.1 or later.
- A v7 artifact needs a runtime that reads v7 and a chain whose configuration
  admits it (`max_rv32_artifact_version` of at least 7).
- v5 and v6 artifacts are still readable (`contract spec` and similar), but
  are not what this CLI produces or verifies.
- Protocol crates move to 0.66.1, the SDK to 0.61.1, and standard interfaces
  to 0.1.75. Newly generated projects pin SDK 0.61.1, the first that records
  the version.
- The lowering metadata and `rv32-lower --json` report `syscall_api_version`.
  The artifact format fingerprint is now `0x95cbfa8b3b2f37da`.

#### Breaking Rust API changes

- `thruput-rv32-artifact`: `FORMAT_VERSION` is 7 and `READABLE_FORMAT_VERSIONS`
  is 5 through 7. `build_artifact_with_spec` and
  `build_artifact_with_extensions` take a leading `SyscallApiVersion`;
  `build_artifact` stamps `SyscallApiVersion::BASELINE`. `HEADER_BYTES` is 120
  and the offsets from `MEMORY_BYTES_OFFSET` on are 8 lower; `V5_V6_HEADER_BYTES`
  is the 128-byte header of v5 and v6. `LEGACY_FORMAT_VERSION_V5` is renamed
  `FORMAT_VERSION_V5`, with `FORMAT_VERSION_V6` beside it. `GAS_TABLE_VERSION`,
  `GAS_SCHEDULE_DIGEST`, `GAS_VERSION_OFFSET`, `GAS_DIGEST_OFFSET`,
  `SYSCALL_ABI_VERSION` and `SYSCALL_ABI_OFFSET` are removed; see
  `SYSCALL_API_MAJOR`, `SYSCALL_API_MAJOR_OFFSET` and
  `SYSCALL_API_MINOR_OFFSET`. `declared_syscall_api` reads the version back.
- `thruput-rv32-lower`: `Lowered` gains `syscall_api`.

### Profiles pin their network hash; the faucet descriptor is gone

The CLI no longer requests `GET /v1/network` from a faucet host. Hosted
profiles used to discover their signing identity from that descriptor, so
`deploy`, `call`, `transfer`, and `airdrop` failed once
`https://faucet.thruput.network` stopped serving it.

- Every profile now needs a `network_hash`, unless it has a local runtime
  manifest. The RPC endpoint is checked against the pin and is never the source
  of the identity. `thruput init` adds the built-in Futurenet pin to an existing
  `futurenet` or `devnet` profile that still uses the built-in RPC URL; run it
  once after upgrading. Other hosted profiles need
  `thruput config set network-hash <hash> --profile <name>`. Without a pin,
  signing commands fail with `missing_network_hash`.
- Hosted airdrops go only to the RPC gateway's `faucet_requestAirdrop`. The CLI
  no longer shows the faucet's amount, cooldown, and quota before the request,
  and no longer compares the amount sent with an advertised one.
- The `faucet_url` profile field is removed. A config that still sets it
  loads, and the key stays in the file untouched, but `config new --faucet-url`
  and `config set`/`unset faucet-url` are rejected; delete a leftover key by
  hand. Hosted profiles no longer require it, and `thruput init` no longer
  writes it. A profile with no `network` and no runtime manifest now sends
  `airdrop` to its RPC endpoint instead of reporting that it is RPC-only.
- Reports no longer carry `faucetUrl`: `config get --json` is schema 3,
  `build --json` is schema 4, and `test --json` is schema 9 (the `profile`
  object in both loses the field). `config get` shows the network hash where
  it showed the faucet URL.
- `airdrop --json` is schema 7: remote reports drop `faucetUrl` and `policy`
  and add `amountTokens`; the remote dry run no longer plans `GET /v1/network`.
- The error codes `faucet_service_error`, `network_descriptor_invalid`, and
  `network_descriptor_mismatch` are removed. An RPC endpoint that serves a
  different network than the pin reports the existing network-mismatch error.
- An airdrop attempt left in flight by an older CLI is not resumed: retained
  attempts are now keyed by profile, recipient, and network hash.

## 0.45.0

- Align protocol crates at 0.66.0, the SDK at 0.61.0, and standard interfaces
  at 0.1.74. Newly generated projects pin SDK 0.61.0.
- Use managed hog-devnode 0.1.33, matching the new protocol identity.

### Breaking Rust API changes

- Rust consumers that exchange protocol or SDK types with the client, testkit,
  or backend crates must update those dependencies together. Types from the
  previous versions are not interchangeable.
- RPC union `Default` payloads now carry typed outcomes and errors. For example,
  use `error: RPCError::kRPCNotFound` instead of its integer `.0` field when
  constructing a result. The numeric codes keep their meaning and encoding.
- These Rust dependency changes do not change the public JSON-RPC wire format.

## 0.44.0 — v6 contract artifacts

- Produce RV32 v6 artifacts with canonical onchain specs in an XDR extension
  list. Readers also accept v5 artifacts, which have no embedded spec.
- The matching runtime accepts both v5 and v6 during the rollout. Existing v5
  contracts remain executable; upgrade all validators before deploying v6.
- Read deployed specs with `thruput inspect` and generate TypeScript bindings
  with `thruput codegen ts --address <address> --output bindings.ts`.
- Use protocol crates 0.65.0, SDK 0.60.1, and interfaces 0.1.73.
- Release the dual-format runtime and matching executor/SDK first, then align
  tools pins and require normal green CI and publication checks. See
  [release instructions](docs/releases.md#gh-554-format-transition).

## 0.43.0

- Generated counter applications and the faucet frontend install their npm
  dependencies from the public registry without a GitHub token.
- Update frontend and browser-harness dependencies to address reported npm
  advisories, and include the faucet's font license notices in production builds.
- Run fork PR checks on GitHub-hosted runners; same-repository PRs and
  maintainer workflows continue to use Namespace. Resume releases from reviewed
  `main` workflow definitions with an explicit existing release tag.
- Add a generated Markdown JSON-RPC reference beside the OpenRPC specification.

### Breaking Rust API changes

- Update `jsonrpsee` from 0.25 to 0.26, including the types re-exported by
  `thruput-client` and `thruput-json-rpc-client`. Applications passing these
  transport types across crate boundaries must use the same dependency version.
  This does not change the JSON-RPC wire format.

## 0.42.0

- Publish every tool crate to crates.io instead of the thruput registry.
  Install the CLI with `cargo install thruput --locked`; no registry
  configuration or token is needed.
- The CLI package is now `thruput`, replacing both `thruput-cli` and the
  separate `thruput` crate that only wrapped it. Its library is still
  `thruput_cli`. Use `cargo install thruput` and `-p thruput` where you used
  `thruput-cli`. On crates.io, `thruput-cli` is a stub that only points to
  `thruput`.
- Update the Rust SDK to `0.58.0` and standard interfaces to `0.1.71`, now
  taken from crates.io. Newly generated projects pin SDK `0.58.0` and resolve
  every dependency from crates.io. Existing projects still name the thruput
  registry; to move one, delete the `[registries]` and `[source]` tables from
  its `.cargo/config.toml` and the `registry = "thruput"` keys from its
  `Cargo.toml`.
- Following SDK `0.58.0`, spec JSON sidecars carry
  `"format": "thruput_contract_spec_v1"` (was `gh_contract_spec_v1`), and an
  RV32 ELF whose raw spec section still has the pre-0.56 `groundhogspecraw`
  name reads as having no contract spec rather than an older spec format.
  `rv32-lower` still strips that section.
- `thruput status` no longer checks for a global thruput registry entry. Its
  JSON report drops `environment.registryConfigured` and
  `environment.cargoConfigPath`, and the `no_registry` finding is gone.
- `hog-firehose-store` no longer recomputes `txs_hash`/`events_hash` Merkle
  roots; it verifies the consensus hash, transaction body hashes and duplicate
  transactions only. `0.41.0` rejected every platform `0.1.31` block with
  `events_hash mismatch` after hogd#4603/#4615 changed the leaf commitment.

### Breaking Rust API changes

Removed public items that nothing constructs or reads. None of them changes
the JSON-RPC wire format.

- `thruput-signing`: `AuthClaimIndex` and `DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS`.
- `thruput-json-rpc-types`: the `balance` module (`BalanceResponse`), the
  `DbHash` alias, and `hex::{Hex0x, TryFromSlice, FixedHexLength}`. Use
  `HexBytes` or `Base58` for byte fields.
- `thruput-json-rpc-server`: `RpcServerError::{TxNotFound, BlockNotFound}`.
  Read backends report a missing transaction or block through their result
  types instead.
- `thruput-node-supervisor`: `NodeSupervisorError::{IndexerIntroRejected,
  DbGatewayIntroRejected, ViewGatewayIntroRejected}`, which intro failures
  never produced (they report `ProtocolIntroRejected`), and
  `HogdDevEndpoints::dev_control`, which was always `None`.
- `thruput-cli`: the never-raised error codes `credentials_empty`,
  `json_unsupported`, `not_implemented`, `manifest_entrypoint_invalid` and
  `wallet_address_derivation_failed`.

## 0.41.0

- Move to protocol `a67b703` through protocol-rs `0.63.0`, now taken from
  crates.io instead of the thruput registry. Tools now interoperates only with
  hogd and chuckd built from the same protocol (platform `0.1.31`). The node
  RPC bindings are now the `thruput-node-rpc` crate, which replaces
  `thruput-scheduler`.
- Default to hog-devnode `0.1.31`, the first managed devnode on protocol
  `a67b703`. Earlier versions remain selectable. Its macOS archive is not
  Developer ID-signed or notarized.
- Update the Rust SDK to `0.57.2` and standard interfaces to `0.1.70`.
  Newly generated projects pin SDK `0.57.2`.
- Bundle the documentation snapshot from `thruput-docs` commit `245ae77`,
  which adds a guide to porting Soroban contracts.
- Use the SDK's generated contract proxies as testkit clients.

## 0.40.2

- Update the Rust SDK to `0.57.1` and standard interfaces to `0.1.69`.
  Newly generated projects pin SDK `0.57.1`.

## 0.40.1

- Bundle the documentation snapshot from `thruput-docs` commit `53198a7`.
- Use Thruput `Z` StrKeys for addresses. Stellar `C` StrKeys are now rejected.
  Update commands and saved address text that use the old prefix. Base58
  identifiers in RPC responses and CLI JSON reports are unchanged.
- Read hogd hosted manifests using `thruput-bootstrap-account-v1` and
  `hosted_dev_account`. The default dev-account signing helper now uses
  `thruput-dev-accounts`, changing its derived signers. Explicit manifest
  seeds remain authoritative.
- Improve generated contract guidance and shared native/guest tests.
- Update zstd dependencies and include license text in published tool crates.
- Add regression coverage for transaction diagnostics, funding, history, and
  local runtime behavior. Report an unrecorded backend rejection as
  inconsistent data instead of a definite transaction rejection.

## 0.40.0

### Testkit typed clients

Tools now uses SDK 0.57.1 and standard interfaces 0.1.69, and newly scaffolded
projects pin SDK 0.57.1. Both releases are compatible with 0.57.0 and 0.1.68.

With the SDK's `remote` feature, `#[contract]` on interface `Foo` generates a
`FooRemote<I>` that passes selectors and encoded arguments to the "invoker". A
testkit `Caller` now serves as its invoker, so a scenario can use the proxy as a
typed client. Methods declared `pure` or `readOnly` return the new
`ContractView`, which simulates the call with the caller as `msg.sender`. Other
methods return `ContractCall`. Use the new `ContractCall::simulate` to read
through a method even if it is not declared read-only.

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

The published `thruput-signing` crate drops helpers with no remaining caller:
`wallet_create_proof_message`, `wallet_create_proof_message_v2` and
`WalletCreateProofV2` (hosted faucet wallet creation is retired),
`create_wallet_calldata`, `dev_wallet_contract` with the
`SigningError::UnsupportedDevWalletArtifact` variant it returned, and
`build_signed_dev_wallet_native_token_transfer` with
`BuildSignedDevWalletNativeTokenTransfer` and
`SignedDevWalletNativeTokenTransfer`. To send native tokens through a dev
wallet, build the transfer with `native_token_transfer_calldata` and
`build_dev_wallet_do_action_transaction`, then sign it with `sign_transaction`.

`thruput-json-rpc-server` no longer implements
`From<RpcServerError> for ErrorObjectOwned`, which produced the pre-wire-14
error codes. The native modules map their errors; for funding errors use
`native::funding::rpc_error`.
`thruput-json-rpc-types` drops the unused `version::VersionInfo`; the
`getVersionInfo` result is `responses::VersionInfo`.
It also drops the pre-wire-14 storage and view result types; use the native
results instead:

| Removed | Replacement |
| --- | --- |
| `state::StorageItemRead` | `responses::StorageResult` (`getStorage`) |
| `state::StorageItemInfo` | `responses::StorageItem` |
| `view::ViewResponse` | `responses::SimulationResult` (`simulateInvocation`) |
| `view::RootInvocationOutput` | `responses::InvocationOutput` |

The native results are not drop-in: `blockHeight` and `height` become
`stateBlockHeight`; simulation events are `responses::EmittedEvent`, whose
`address` replaces `contractAddress`; `NonnegativeInt.value` is an `i128` (still
a decimal string on the wire); unit variants such as `Fault` become empty struct
variants (`Fault {}`); the native enums add an `Unsupported` variant; and there
is no `From` conversion from the hogd wire storage item.

`thruput-json-rpc-client` drops its `receipt` module: the report-model
`ReceiptSource` adapter, `poll_receipt`, `ReceiptPollConfig::within`/`attempts`
and the `DEFAULT_RECEIPT_POLL_*` constants. Wait for receipts with
`native::receipt::poll_receipt` (re-exported at the crate root), which keeps
commitment evidence and is configured by a timeout and interval.

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
