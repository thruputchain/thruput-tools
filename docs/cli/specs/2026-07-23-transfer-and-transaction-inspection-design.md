# HOG Transfer and Transaction Inspection
**Date:** 2026-07-23 **Status:** Transfer implemented; transaction inspection ready for implementation

**Companions:**

- `2026-06-25-remote-devnet-faucet-design.md` - hosted network metadata, faucet flows, and saved-wallet behavior.

- `2026-05-07-transaction-detail-options.md` - deferred deep transaction detail and replay analysis.

- `../roadmap.md` - current CLI transaction and read surfaces.

## Goal
Close the basic wallet loop and expose the transaction data already available through the public RPC:

```bash
groundhog transfer alice 10.5 --profile devnet
groundhog tx show 0x0123...
groundhog tx list alice --limit 25
```

The workstream adds:

1. native HOG transfers from the selected signing wallet;

2. one-shot inspection of a transaction hash;

3. paginated transaction history for an address.


The commands should work consistently against local and hosted profiles, use the existing profile/RPC override model, and provide clear human output plus stable JSON output.
## Non-goals
This work does not add:

- arbitrary token transfers or an SPL-style token CLI;

- `ALL` amount handling;

- transfer batching, memos, scheduled transfers, or multisig;

- raw transaction-body retrieval or decoding;

- deep execution traces, replay, gas profiling, or retained storage deltas;

- event searching beyond events already included in a transaction receipt;

- incoming-transfer history inferred from nested calls;

- an unbounded `tx list --all` crawl;

- new public RPC methods.


Deep post-hoc detail remains governed by `2026-05-07-transaction-detail-options.md`.
## Current capabilities and constraints
The CLI already contains nearly all transfer-building primitives:

- `airdrop` and wallet prefunding construct native HOG transfers;

- `hog.rs` defines the HOG address, decimal scale, transfer entrypoint, and calldata encoding;

- `deploy` and `call` resolve the selected signer and wallet, build a wallet-wrapped transaction, sign it, submit it, and poll for a receipt;

- `tx_utils.rs` owns tip-derived expiry, submission, polling, and execution failure mapping;

- `balance` resolves saved wallets, contract aliases, direct addresses, and manifest-backed `dev-N` addresses.


The public RPC already exposes:

```text
groundhog_getTransactionStatus
groundhog_getTransactionReceipt
groundhog_getTransactionsByAddress
```

The shared API definitions are in `crates/json-rpc/json-rpc-api/src/lib.rs`; status, history, and receipt models are in `crates/json-rpc/json-rpc-types/src/transaction.rs` and `receipt.rs`.

Important limits:

- address history contains transactions whose **root invocation** targets the queried address;

- it does not index every address touched by nested execution;

- the history response contains transaction hashes and block heights, not raw transaction bodies;

- a receipt contains execution status, events, return data, and abort data, but not sender, recipient, amount, calldata, or a deep trace;

- local history and receipts require a devnode exposing the indexer read plane; reduced local backends may return `NotImplemented`.


For a wallet-wrapped HOG transfer, the root invocation is the sender's wallet. The transfer therefore appears in the sender wallet's history. The recipient and nested HOG contract are not automatically discoverable through `getTransactionsByAddress`.
## Decisions
| # | Decision | Rationale |
|---|---|---|
| D1 | Add top-level `groundhog transfer` for native HOG. | Funding and balance without sending leaves the wallet workflow incomplete. |
| D2 | Group inspection under `groundhog tx show` and `groundhog tx list`. | A transaction namespace can grow without overloading contract-oriented `inspect`. |
| D3 | Transfer uses the active profile's effective default key as signer and resolves its source wallet through explicit, profile-default, or unique-owner selection. | Keys and wallet names are independent, and one key may own several wallets. Selection must never switch signers implicitly. |
| D4 | `transfer --dry-run` builds and signs but does not submit. | This matches existing deploy/call dry-run semantics. |
| D5 | `tx show` composes public status and receipt RPCs only. | The optional dev-detail method is not a stable public inspection contract. |
| D6 | `tx list` returns one newest-first page of lightweight references. | Automatic hydration creates an N+1 RPC pattern; users can inspect selected hashes with `tx show`. |
| D7 | History limitations are explicit in help and output. | Empty incoming history must not be mistaken for proof that an address received no nested transfers. |
| D8 | Semantic statuses such as `unknown` and `invalid` are successful query results. | The lookup succeeded even when the transaction did not commit. |
| D9 | Native HOG address, decimals, and transfer entrypoint are protocol constants. | These values do not vary by network, so descriptor resolution would add failure modes without adding correctness. |
| D10 | A broadcast transfer that cannot be confirmed retains its locally computed hash and exits nonzero. | The user must be able to inspect a possibly accepted transaction without risking a duplicate transfer through a blind retry. |
| D11 | `tx list` output omits traversal order. | Initial CLI requests are newest-first, but an opaque continuation cursor may carry an order the response does not expose. |
| D12 | Add profile-scoped `default_wallet`, `transfer --from`, and shared deploy/call `--wallet` selection. | All transaction commands need one consistent way to resolve multi-wallet ambiguity. |
## Command surface
### `groundhog transfer <RECIPIENT> <AMOUNT>`
Transfer native HOG from the effective signing wallet.

```bash
groundhog transfer alice 10.5
groundhog transfer counter 1 --profile devnet
groundhog transfer 0xabcd... 0.0000001 --dry-run
groundhog transfer dev-1 25 --profile local
```
Options:

```text
--dry-run
--profile <PROFILE>
--rpc-url <URL>
--runtime-manifest <PATH>
--from <WALLET>
```

`--from` accepts a saved wallet name in the selected profile. It does not accept
an address and does not select a signing key. Deploy and call expose the same
shared source-wallet selector as `--wallet <WALLET>`.

Profiles may configure a default source wallet:

```toml
[profile.devnet]
default_key = "default"
default_wallet = "dev-default-1"
```

Resolve the source wallet in this order:

1. the command's explicit `--from` or `--wallet` value;

2. the merged profile's `default_wallet`;

3. the only saved wallet owned by the effective signer;

4. otherwise, a typed missing or ambiguous-wallet error.

An explicit or configured wallet must exist and its `owner_public_key` must
match the effective key. Wallet selection never searches for or switches to a
different private key. Manifest-backed `dev-N` keys retain their fixed runtime
wallet and reject explicit or configured saved-wallet selection.

Automatic owner matching ignores malformed ownership metadata because it
cannot associate that binding with the effective signer. If no valid binding
matches, selection returns the missing-wallet error. Explicit and configured
wallets instead report their malformed ownership metadata directly.

Recipient resolution

Resolve the recipient in this order:

1. saved wallet name;

2. contract alias;

3. direct 32-byte `0x` address;

4. manifest-backed `dev-N` address.


If a name exists as both a saved wallet and contract alias, the saved wallet wins. This matches the broad target precedence already used by `balance`.

The resolved recipient may be an address with no deployed contract. Native HOG balances are address-keyed, so funding a future wallet address is valid.

Sender resolution

Resolve the sender using the established transaction path:

1. choose the effective profile;

2. choose the profile default key, global default key, or valid local `dev-0` fallback;

3. resolve an explicit source wallet, the profile's `default_wallet`, or the
   signer's unique saved wallet;

4. verify that a selected saved wallet's owner public key matches the local signer;

5. resolve wallet entrypoints through the existing runtime-manifest, hosted-descriptor, or legacy-default path used by `deploy` and `call`.


A non-dev key without a usable saved wallet fails with the same actionable guidance as deploy/call: create or select a wallet before sending.

After resolving both sides, reject a transfer whose sender and recipient are the same address. This validation runs before RPC access, transaction construction, signing, or submission so a no-op token transfer cannot consume a transaction fee.

Amounts

`AMOUNT` is a user-facing decimal HOG amount. Parsing is exact and checked.

With HOG's fixed seven-decimal scale, accept:

```text
1
1.0
0.0000001
10.5000000
```

Trim surrounding whitespace with Rust's `str::trim`, matching the current airdrop behavior. Whitespace inside the value remains invalid.

Reject:

- empty values;

- signs, including `+1` and `-1`;

- zero;

- exponent notation;

- more than seven fractional digits;

- non-digits other than one decimal point;

- values whose raw-unit conversion overflows `u128`.


The minimum amount is one raw unit. Output preserves both a canonical HOG decimal string and a raw-unit decimal string. Do not use floating point.

Move the existing airdrop amount parsing/formatting into shared native-HOG helpers so airdrop and transfer cannot drift.

Native HOG constants

Native HOG uses protocol-defined constants on every network:

- token address: `0x0000000000000000000000000000000000000000000000000000000000000001`;

- decimals: seven;

- transfer entrypoint: the canonical native-HOG transfer selector.


Keep these values in the existing `hog.rs` module and use them from `balance`, `airdrop`, wallet prefunding, and `transfer`. Do not add a second metadata abstraction. This decision supersedes the companion descriptor design's `nativeHog` fields as a CLI transaction-construction source; those redundant fields may remain for compatibility but are not consulted here.

Transaction construction

Build the same wallet-wrapped HOG invocation used by existing account funding:

```text
sender wallet doAction
  -> native HOG transfer(recipient, raw amount)
```

Reuse:

- canonical transfer calldata encoding;

- wallet entrypoints from local manifest or hosted descriptor;

- the frozen legacy wallet entrypoints when the existing resolver selects that path;

- tip-derived block expiry;

- transaction nonce generation;

- source-address authentication and local signing;

- `groundhog_sendRawTransaction` submission;

- the existing submit and poll client operations, called separately so transfer retains its hash;

- existing execution-status failure mapping.


Do not add a preflight balance rejection. A balance read can race another transaction and is not authoritative. A committed bounds failure or recent conflict diagnostic should be surfaced through normal transaction handling.

Dry run

`--dry-run` resolves the wallet entrypoints, queries the chain tip, builds and signs the transaction, and returns the prepared transaction without broadcasting it. It is not a simulation and does not predict execution success.

Dry-run output clearly includes:

- `submitted: false`;

- sender wallet and signer public key;

- resolved recipient;

- token address;

- canonical and raw amount;

- expiry height;

- transaction hash and signed transaction encoding.


No `sendRawTransaction`, status, or receipt request may occur after preparation.

Live result

A live transfer succeeds only when the committed receipt has `ExecStatus::Ok`. Other committed statuses use the existing command-owned failure result and nonzero exit behavior, retaining the hash, height, receipt status, return or abort data, and emitted events where available.

Compute the canonical transaction hash before submission. The hash returned by `sendRawTransaction` must match it, and a polled receipt's hash must match it. Treat either mismatch as an invalid RPC response and emit the inconclusive command-owned failure using only the locally computed hash; never poll, report success, or advise inspection using a conflicting hash.

For transfer data, `submitted: true` means the CLI invoked the `sendRawTransaction` RPC method; it does not prove that a request reached the server or that the endpoint accepted the transaction. This distinction is observable at the client boundary and covers connection/transport failures and a lost submission response.

Live outcomes are:

1. A matching receipt is available. `ExecStatus::Ok` succeeds; every other execution status emits a command-owned failure with the full receipt context.

2. Status is terminal `invalid`. Emit a command-owned failure with the hash, `confirmation: invalid`, and the complete `TransactionStatus::Invalid` object. This is conclusive and must not include the "may still commit" warning.

3. The outcome remains inconclusive because the submission call had an ambiguous error or the poll operation failed or exhausted. Emit a command-owned failure with the locally computed hash and `confirmation: unavailable`. Include `transactionStatus` only when the poll operation returns an advisory `recentFailure`; do not require the poller to expose intermediate statuses it does not retain. Human output must warn that the transaction may still commit and advise retaining the hash and verifying its status before resubmitting. Once transaction inspection ships, the guidance may name `groundhog tx show <TX_HASH>` directly.


Use stable command-error code `transaction_invalid` for outcome 2 and `transaction_confirmation_unavailable` for outcome 3. Existing execution-status error codes remain authoritative for committed failures.

Classify `sendRawTransaction` errors from their stable JSON-RPC codes:

- `InvalidParams`, `TxInvalid`, `TxRejected`, `BackendUnavailable`, `NotImplemented`, and `Overloaded` are conclusive pre-acceptance failures under their documented RPC semantics and use the standard error envelope;

- `BackendTimeout`, unknown server errors, and client transport errors have an ambiguous outcome and use outcome 3;

- failure to construct the RPC client occurs before method invocation and uses the standard error envelope without transfer data.

### `groundhog tx show <TX_HASH>`
Inspect the public status and receipt for one transaction:

```bash
groundhog tx show 0x0123...
groundhog tx show 0x0123... --profile devnet --json
```

Options:

```text
--profile <PROFILE>
--rpc-url <URL>
--runtime-manifest <PATH>
```

The command performs a one-shot lookup; it does not wait for a pending transaction to commit in v1.

Lookup flow:

1. validate and canonicalize the transaction hash;

2. call `groundhog_getTransactionStatus`;

3. if status is `committed`, request `groundhog_getTransactionReceipt`;

4. render both results without inventing unavailable fields.


Represent every public status:

- `pending`;

- `committed { height }`;

- `invalid { reason }`;

- `recentFailure { attemptHeight, conflict }`;

- `unknown`;

- forward-compatible `other`.


Current shared RPC types normalize an unrecognized future status or execution result to `other` and discard its unknown payload. Output preserves that normalized state; it cannot preserve fields the typed client does not retain.

`pending`, `invalid`, `recentFailure`, and `unknown` are query results and exit successfully. Transport, RPC, validation, and capability failures exit nonzero.

If status is committed but a successful receipt RPC returns no receipt, preserve the committed status and height and show `receipt: unavailable`; JSON uses `receipt: null`. Do not downgrade it to unknown or fabricate an execution result. A receipt RPC error, including `NotImplemented`, remains a command failure and must not be converted to `receipt: null`.

When a receipt is present, its transaction hash must match the requested hash and its height must match the committed status height. Treat a mismatch as an invalid RPC response and exit nonzero rather than choosing one response as authoritative.

When present, show receipt fields:

- transaction hash;

- committed height;

- execution status and reason fields;

- emitted events;

- raw return data;

- raw abort data.


Return and abort bytes remain raw because a transaction hash alone does not identify a local contract spec reliably. Typed decoding can be a future option when the caller supplies an artifact/spec.

The command must not call `groundhog_dev_getTransactionDetail`. That method is optional, untyped at the public boundary, and outside the retained-data decision for this workstream.
### `groundhog tx list <ADDRESS>`
List one page of transaction references for an address:

```bash
groundhog tx list alice
groundhog tx list counter --limit 25
groundhog tx list 0xabcd... --cursor 0x0123...
```

Options:

```text
--limit <COUNT>
--cursor <OPAQUE_HEX>
--profile <PROFILE>
--rpc-url <URL>
--runtime-manifest <PATH>
```

Address resolution uses the same saved-wallet, contract-alias, direct-address, and `dev-N` precedence as transfer recipients.

The first release always requests descending order anchored at the end of the indexed range when the cursor is empty. A non-empty cursor continues the server-authored traversal; its embedded order and anchor are authoritative. Because the response does not expose those values, CLI output does not report an order.

Defaults and limits:

- default page size: 50;

- `--limit` must parse as a nonzero `u32`; endpoint request-size validation remains authoritative, and a response containing more references than requested is rejected as invalid;

- the CLI validates cursor hex and decodes it into the existing variable-length `PaginationCursor`; endpoint cursor-size validation remains authoritative.


The response includes:

- original target and resolved address;

- `{ txHash, height }` references;

- opaque continuation cursor;

- indexer's latest retained/processed block;

- indexer's oldest retained block.


Do not automatically fetch a receipt for every reference. An empty reference array with an empty cursor is normal exhaustion. An empty array with a non-empty cursor is also valid if the backend advances a sparse/selective scan; callers may continue with that cursor.

Human help and output must state that history matches root invocations only. Specifically, querying a transfer recipient does not guarantee that incoming wallet-wrapped transfers will appear. The JSON envelope carries the same caveat as a `root_invocation_history_only` warning so machine consumers do not silently interpret the page as a complete asset ledger.
## Output contracts
All three commands use the existing `CommandResult` envelope. Command names are:

```text
transfer
tx.show
tx.list
```

Each command data object uses `schemaVersion: 1`, camelCase field names, and canonical lowercase `0x` hex. Raw `u128` values serialize as decimal strings. JSON data fields and command-error codes are the versioned machine contract; error messages, hints, details, and human wording are diagnostic and may evolve while remaining deterministic, script-clean, and semantically equivalent.

Transfer data includes `senderWallet`: the selected saved-wallet name for stored
keys, or `null` for manifest-backed `dev-N` senders.
### Transfer data
Live success data:

```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "dryRun": false,
  "submitted": true,
  "sender": "0x...",
  "senderWallet": "dev-default-1",
  "signerPublicKey": "0x...",
  "recipientInput": "alice",
  "recipient": "0x...",
  "tokenAddress": "0x...0001",
  "amount": "10.5",
  "rawAmount": "105000000",
  "txHash": "0x...",
  "height": 123,
  "status": { "result": "ok" },
  "events": []
}
```

Live success and committed failure preserve `returnData` and `abortData` when present in the receipt, regardless of execution status.

Committed live data omits `confirmation`, `transactionStatus`, `expiryHeight`, and `signedTransaction`.

Dry-run data:

```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "dryRun": true,
  "submitted": false,
  "sender": "0x...",
  "senderWallet": "dev-default-1",
  "signerPublicKey": "0x...",
  "recipientInput": "alice",
  "recipient": "0x...",
  "tokenAddress": "0x...0001",
  "amount": "10.5",
  "rawAmount": "105000000",
  "txHash": "0x...",
  "expiryHeight": 1123,
  "signedTransaction": "0x..."
}
```

Dry-run data omits `height`, `status`, `events`, `returnData`, `abortData`, `confirmation`, and `transactionStatus`.

Broadcast-but-unconfirmed data:

```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "dryRun": false,
  "submitted": true,
  "sender": "0x...",
  "senderWallet": "dev-default-1",
  "signerPublicKey": "0x...",
  "recipientInput": "alice",
  "recipient": "0x...",
  "tokenAddress": "0x...0001",
  "amount": "10.5",
  "rawAmount": "105000000",
  "txHash": "0x...",
  "confirmation": "unavailable"
}
```

Broadcast-but-unconfirmed data is emitted in an `ok: false` command-owned result on stdout and exits nonzero. It omits `height`, `status`, `events`, `returnData`, `abortData`, `expiryHeight`, and `signedTransaction`; the envelope error code is `transaction_confirmation_unavailable`. When the poll operation returns a `recentFailure`, add its complete shared RPC representation as `transactionStatus`. Absence of `transactionStatus` means no semantic status was retained, not `unknown`.

Terminal invalid data uses the same common live fields with `confirmation: "invalid"` and the complete shared `TransactionStatus::Invalid` representation in `transactionStatus`. It is emitted in an `ok: false` command-owned result with error code `transaction_invalid` and exits nonzero.

```json
{
  "confirmation": "invalid",
  "transactionStatus": {
    "status": "invalid",
    "reason": "..."
  }
}
```

The snippet shows the invalid-only fields; all common live fields through `txHash` remain required. `height`, receipt fields, `expiryHeight`, and `signedTransaction` are omitted.

Committed execution failure follows deploy/call precedent: emit an `ok: false` command-owned JSON record on stdout and exit nonzero. Its data has the live shape above, including `height`, `status`, and `events`, with `returnData` and `abortData` when present. Preflight and parsing failures, client-construction failures before method invocation, and the conclusive `sendRawTransaction` rejection codes listed above use the standard error object on stderr.
### Transaction-show data
```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "txHash": "0x...",
  "status": { "status": "committed", "height": 123 },
  "receipt": {
    "txHash": "0x...",
    "height": 123,
    "status": { "result": "ok" },
    "events": []
  }
}
```

`receipt` is always present in the data schema and nullable. It is `null` for every non-committed status and when a committed status is followed by a successful receipt lookup returning no receipt. Preserve the RPC status and receipt structures rather than flattening them. Absent optional receipt fields remain omitted, matching the shared receipt serialization. Unknown variants use the shared type's lossy `other` normalization.
### Transaction-list data
```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "targetInput": "alice",
  "address": "0x...",
  "refs": [
    { "txHash": "0x...", "height": 123 }
  ],
  "cursor": "0x...",
  "latestBlock": 130,
  "oldestBlock": 0
}
```

Human output should make a non-empty continuation cursor easy to reuse and report `latestBlock` and `oldestBlock`. The command does not fetch the chain tip or calculate indexer lag in v1.
## Validation and errors
Add stable errors or reuse existing typed errors for:

- missing effective signing key;

- missing or mismatched saved wallet;

- unknown recipient/address target;

- malformed direct address;

- invalid HOG amount and excessive precision;

- raw amount overflow;

- malformed or wrong-length transaction hash;

- invalid cursor encoding;

- invalid history limit;

- unsupported backend capability;

- RPC unavailable, timeout, overload, or rejected request;

- transaction submission, polling, or committed execution failure.


Input parsing happens before RPC where possible. Direct CLI addresses require a `0x` or `0X` prefix followed by exactly 64 case-insensitive hexadecimal digits. The shared target resolver must handle either prefix before fixed-size address parsing; this corrects the current balance path's incomplete `0X` handling. Transaction hashes accept the shared fixed-size RPC hex type's prefixed or unprefixed forms. Cursors accept the shared variable-length RPC hex form and decode into `PaginationCursor`. Output fields containing resolved addresses, transaction hashes, and cursors use canonical lowercase `0x` hex. `recipientInput` and `targetInput` preserve the user's argument verbatim.

Do not convert `NotImplemented` into an empty receipt or history page. Explain that the selected endpoint does not expose the required indexer capability and suggest using a full local devnode or compatible remote RPC.

Treat JSON-RPC `MethodNotFound` and Groundhog `NotImplemented` as the same unsupported-capability class for these commands. Other unavailable, timeout, overload, and rejected-request responses remain RPC failures; do not build a broader capability-discovery layer.

Important edge cases:

- aliases and wallet names may collide; wallet precedence is deterministic;

- a direct recipient does not need a deployed wallet;

- one raw unit is valid, zero is not;

- a sender's balance may change after transaction construction;

- the transaction can expire before submission or commit;

- a transaction can be pending, later invalid, recently fail from a conflict, or commit with a deterministic execution failure;

- `recentFailure` is best-effort and advisory, may disappear, and may later be superseded by a committed retry of the same transaction;

- status can report committed while receipt hydration is briefly unavailable;

- a foreign or stale pagination cursor may be rejected by the backend;

- indexer retention can make `oldestBlock` greater than zero;

- root-invocation history is not a complete incoming/outgoing asset ledger;

- unknown future status or receipt variants normalize to the shared `other` representation.

## Security considerations
- Never print private keys or seed material.

- The signed dry-run transaction is potentially submit-capable until expiry; label it accordingly and emit it only because existing deploy/call dry runs establish that contract.

- Resolve wallet entrypoints through the same validated manifest/descriptor or frozen legacy-default path used by existing signed flows; use protocol constants for native HOG.

- Keep transaction hashes and cursors as untrusted input; validate fixed hash length and hex encoding before RPC, then rely on the endpoint's configured bound for opaque cursor length.

- Escape control and bidirectional-formatting characters in untrusted terminal text without obscuring ordinary printable output.

- Treat event, return, abort, status-reason, and backend strings as untrusted display data. Do not interpret them as terminal markup or instructions.

- Do not claim a successful transfer before an `ExecStatus::Ok` receipt.

## Backend compatibility
No RPC schema change is required.

Expected behavior by endpoint capability:

| Capability | Transfer | `tx show` | `tx list` |
|---|---|---|---|
| Submit + chain + indexer | Full support | Full support | Full support |
| Submit + chain, no indexer | Broadcast may succeed; report confirmation unavailable with the retained local hash when receipt polling is unavailable | `NotImplemented` or backend unavailable | `NotImplemented` or backend unavailable |
| Read-only indexer endpoint | No transfer | Committed hashes may be inspectable; pending/unknown status can be unavailable without scheduler access | History supported |
| RPC-only profile with a saved wallet but no manifest/descriptor | Transfer uses the same frozen legacy wallet entrypoint defaults as deploy/call | Supported if read methods exist | Supported if read methods exist |
| RPC-only profile with no usable saved wallet | Fails before signing | Supported if read methods exist | Supported if read methods exist |

The CLI should report the missing capability, not infer support from profile names such as `local` or `devnet`.
## Implementation shape
Keep the work primarily inside `crates/tools/cli` and reuse generated RPC clients and shared types.

Expected areas:

- parser, command names, and dispatch in `src/main.rs`;

- a `transfer` command module;

- a `tx` command module for show/list orchestration;

- the existing `hog.rs` constants/encoding plus the airdrop amount parser/formatter moved there;

- shared broad address resolution instead of command-specific copies;

- existing account/wallet resolution and signing builders;

- existing client submit and poll operations, orchestrated separately in transfer to retain and compare the locally computed and server-returned hashes;

- stable versioned JSON report types;

- command-specific errors and hints.


Do not copy or generalize the airdrop transaction builder. Reuse `hog.rs` calldata encoding and the existing wallet transaction builder directly. Move only the amount parser/formatter into `hog.rs`, where `airdrop` and `transfer` immediately share it; leave airdrop, wallet-prefunding, and transfer policy separate.

Keep each command as a thin orchestrator. Do not introduce a generalized transaction service, pagination framework, capability model, or new cross-crate abstraction. Expose the smallest existing target/HOG helpers needed by the new commands; generalize only when an existing caller can immediately share the result.

No gateway, RPC API, or protocol change is expected. Hermetic binary tests use mock RPC responses for non-empty history and pagination. Process-level transfer/history end-to-end tests require a compatible external `hog-devnode` and belong in the existing ignored smoke path, not normal hermetic CI. Extend the fake indexer only if an existing supervisor integration test needs the same non-empty fixture.
## Phasing
### Phase 1: transfer
- move the checked HOG amount parser and formatter into the existing `hog.rs` module;

- shared recipient resolution;

- transfer dry-run and live flow;

- human/JSON output and failure handling.

### Phase 2: transaction inspection
- `tx show` status and receipt composition;

- `tx list` newest-first page and cursor continuation;

- capability-aware errors;

- human/JSON output;

- local and hosted integration coverage.


The phases can be implemented in parallel after shared target/RPC plumbing is agreed because inspection does not depend on transfer.
## Acceptance criteria
1. A configured local or hosted wallet can transfer native HOG to a saved wallet, contract alias, direct address, or manifest-backed `dev-N` address.

2. Transfer uses the effective signer wallet, protocol-defined native-HOG constants, exact amount conversion, and existing expiry/sign/submit/poll semantics.

3. Transfer dry-run makes no submission and clearly returns a prepared, submit-capable signed transaction.

4. Live transfer exits successfully only after a matching `ExecStatus::Ok` receipt, retains receipt context on execution failure, and retains the locally computed hash whenever confirmation is unavailable.

5. `tx show` accurately represents pending, committed, invalid, recent-failure, unknown, future-status, and committed-without-receipt states.

6. `tx list` resolves supported address forms, returns one newest-first page, and preserves cursor and indexer retention metadata.

7. No inspection command uses the optional dev-detail method or claims fields unavailable from public RPC.

8. Root-invocation-only history semantics are visible in help and documentation.

9. Unsupported endpoint capabilities produce actionable errors rather than empty results.

10. JSON output remains deterministic and versioned; human and JSON output stay cleanly separated from status/error streams.

## Test plan
### Unit tests
- valid HOG decimal forms, minimum unit, canonical formatting, excess precision, signs, exponent notation, zero, and overflow;

- recipient/address precedence and collision behavior;

- prefixed mixed-case direct-address input and canonical lowercase output;

- signer-wallet ownership validation;

- native-HOG constants and fixed seven-decimal behavior;

- transaction hash, cursor, and limit parsing;

- `tx show` state composition, including committed status without a receipt;

- stable JSON serialization for dry-run, success, execution failure, terminal invalid, confirmation-unavailable failure, and semantic inspection statuses.

### Binary integration tests
Use mock RPC fixtures to cover:

- transfer dry-run with no submit request;

- live transfer success and exact signed inner HOG invocation;

- committed revert/deterministic failure, terminal invalid, advisory recent failure, lost submission response, and confirmation-unavailable timeout or backend failure with the retained local hash;

- server-returned hash mismatch, receipt hash mismatch, and `tx show` receipt hash/height mismatch;

- profile and RPC overrides;

- every public transaction status;

- receipt events, return data, and abort data;

- empty, non-empty, and continued history pages;

- malformed input and `NotImplemented` responses;

- human output and JSON stream discipline.

### End-to-end tests
Run these through the ignored external-devnode smoke path:

- local devnode HOG transfer updates sender and recipient balances;

- the resulting hash is inspectable through `tx show`;

- sender-wallet history contains the transaction;

- cursor pagination returns stable non-overlapping references;

- recipient history behavior matches the documented root-invocation limitation.

## Deferred follow-ups
- `tx show --wait` or a separate confirmation command;

- transaction-body retrieval and decoding if the RPC adds it;

- typed return/abort decoding with an explicitly supplied contract spec;

- event search and follow mode;

- `ALL`, batch transfers, arbitrary token standards, and allowance flows;

- incoming asset history backed by event or token indexes;

- deep trace/replay and storage-delta inspection;

- automatic multi-page history traversal.
