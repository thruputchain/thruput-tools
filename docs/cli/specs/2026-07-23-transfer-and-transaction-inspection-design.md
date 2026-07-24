# HOG Transfer and Transaction Inspection
**Date:** 2026-07-23 **Status:** Design ready for implementation planning

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
  

The commands should work consistently against local and hosted profiles, use the existing profile/RPC override model, and provide stable human and JSON output.
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
  
- `hog.rs` defines the HOG address fallback, decimal scale, transfer entrypoint, and calldata encoding;
  
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
| D3 | Transfer uses the active profile's effective default key and associated wallet. | This matches deploy and call and avoids introducing a second signer-selection model. |
| D4 | `transfer --dry-run` builds and signs but does not submit. | This matches existing deploy/call dry-run semantics. |
| D5 | `tx show` composes public status and receipt RPCs only. | The optional dev-detail method is not a stable public inspection contract. |
| D6 | `tx list` returns one newest-first page of lightweight references. | Automatic hydration creates an N+1 RPC pattern; users can inspect selected hashes with `tx show`. |
| D7 | History limitations are explicit in help and output. | Empty incoming history must not be mistaken for proof that an address received no nested transfers. |
| D8 | Semantic statuses such as `unknown` and `invalid` are successful query results. | The lookup succeeded even when the transaction did not commit. |
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
```

The command does not add `--from` in v1. It uses the same effective default-key and saved-wallet resolution as `deploy` and `call`. If multiple-wallet selection is added later, transfer should adopt the same shared option rather than invent one first.

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
  
3. resolve the wallet contract associated with that signer;
  
4. verify that a saved wallet's owner public key matches the local signer;
  
5. load local or hosted wallet metadata needed to build `doAction`.
  

A non-dev key without a usable saved wallet fails with the same actionable guidance as deploy/call: create or select a wallet before sending.

Amounts

`AMOUNT` is a user-facing decimal HOG amount. Parsing is exact and checked.

For the current seven-decimal fallback, accept:

```text
1
1.0
0.0000001
10.5000000
```

Trim surrounding ASCII whitespace before parsing, matching the current airdrop amount behavior. Whitespace inside the value remains invalid.

Reject:

- empty values;
  
- signs, including `+1` and `-1`;
  
- zero;
  
- exponent notation;
  
- more fractional digits than the selected network's HOG decimals;
  
- non-digits other than one decimal point;
  
- values whose raw-unit conversion overflows `u128`.
  

The minimum amount is one raw unit. Output preserves both a canonical HOG decimal string and a raw-unit decimal string. Do not use floating point.

Move the existing airdrop amount parsing/formatting into shared native-HOG helpers so airdrop and transfer cannot drift.

Native HOG metadata

Hosted profiles should use authoritative native-token metadata from the fetched network descriptor, including token address, decimals, and transfer entrypoint. Local profiles use runtime metadata when available. The current `0x...01`, seven decimals, and standard transfer selector remain a documented local/RPC-only fallback until every endpoint advertises native-token metadata.

Validate hosted descriptor metadata before signing:

- token address is exactly 32 bytes;
  
- transfer entrypoint is exactly 8 bytes;
  
- decimals are at most 38 and `10^decimals` fits in `u128`;
  
- all required fields are present and parseable.
  

Authoritative metadata is not required to equal the fallback constants. A malformed hosted descriptor is a network configuration error, not a reason to silently fall back.

All user-facing native-HOG commands must share the same metadata and formatting path. If a network advertises decimals other than seven, `balance`, `airdrop`, wallet-prefunding diagnostics, and `transfer` must not display conflicting units. Normalize those existing callers as part of extracting the shared helper.

Transaction construction

Build the same wallet-wrapped HOG invocation used by existing account funding:

```text
sender wallet doAction
  -> native HOG transfer(recipient, raw amount)
```

Reuse:

- canonical transfer calldata encoding;
  
- wallet entrypoints from local manifest or hosted descriptor;
  
- tip-derived block expiry;
  
- transaction nonce generation;
  
- source-address authentication and local signing;
  
- `groundhog_sendRawTransaction` submission;
  
- receipt polling and timeout behavior;
  
- existing execution-status failure mapping.
  

Do not add a preflight balance rejection. A balance read can race another transaction and is not authoritative. A committed bounds failure or recent conflict diagnostic should be surfaced through normal transaction handling.

Dry run

`--dry-run` resolves all metadata, queries the chain tip, builds and signs the transaction, and returns the prepared transaction without broadcasting it. It is not a simulation and does not predict execution success.

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

If status is committed but the receipt is temporarily absent, preserve the committed status and height and show `receipt: unavailable`. Do not downgrade it to unknown or fabricate an execution result.

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

The first release always requests descending order anchored at the end of the indexed range. A non-empty cursor continues the server-authored traversal; its embedded order and anchor are authoritative.

Defaults and limits:

- default page size: 50;
  
- maximum accepted CLI limit: 200 as a conservative public-client bound;
  
- endpoints configured with a lower limit may reject the request and their RPC validation remains authoritative;
  
- the CLI validates cursor hex and applies a 256-byte local safety bound;
  
- endpoints configured with a lower cursor bound may still reject it;
  
- `--limit 0` is rejected at the CLI rather than relying on the RPC's server-default sentinel.
  

The response includes:

- original target and resolved address;
  
- `{ txHash, height }` references;
  
- opaque continuation cursor;
  
- indexer's latest retained/processed block;
  
- indexer's oldest retained block.
  

Do not automatically fetch a receipt for every reference. An empty reference array with an empty cursor is normal exhaustion. An empty array with a non-empty cursor is also valid if the backend advances a sparse/selective scan; callers may continue with that cursor.

Human help and output must state that history matches root invocations only. Specifically, querying a transfer recipient does not guarantee that incoming wallet-wrapped transfers will appear.
## Output contracts
All three commands use the existing `CommandResult` envelope. Command names are:

```text
transfer
tx.show
tx.list
```

Each command data object uses `schemaVersion: 1`, camelCase field names, and canonical lowercase `0x` hex. Raw `u128` values serialize as decimal strings.
### Transfer data
Representative live success:

```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "dryRun": false,
  "submitted": true,
  "sender": "0x...",
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

Dry-run data replaces receipt fields with prepared transaction metadata and the signed transaction encoding.

Execution failure follows deploy/call precedent: emit an `ok: false` command-owned JSON record containing receipt context on stdout and exit nonzero. Preflight, parsing, transport, and RPC errors use the standard error object on stderr.
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

`receipt` is nullable. Preserve the RPC status and receipt structures rather than flattening them. Absent optional receipt fields remain omitted, matching the shared receipt serialization. Unknown variants use the shared type's lossy `other` normalization.
### Transaction-list data
```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "targetInput": "alice",
  "address": "0x...",
  "order": "descending",
  "refs": [
    { "txHash": "0x...", "height": 123 }
  ],
  "cursor": "0x...",
  "latestBlock": 130,
  "oldestBlock": 0
}
```

Human output should make a non-empty continuation cursor easy to reuse and should report indexer lag context when `latestBlock` is behind a separately known chain tip. The command need not issue an extra chain-tip request solely to calculate lag in v1.
## Validation and errors
Add stable errors or reuse existing typed errors for:

- missing effective signing key;
  
- missing or mismatched saved wallet;
  
- unknown recipient/address target;
  
- malformed direct address;
  
- invalid HOG amount and excessive precision;
  
- raw amount overflow;
  
- invalid native-token metadata;
  
- malformed or wrong-length transaction hash;
  
- invalid cursor encoding or excessive cursor size;
  
- invalid history limit;
  
- unsupported backend capability;
  
- RPC unavailable, timeout, overload, or rejected request;
  
- transaction submission, polling, or committed execution failure.
  

Input parsing happens before RPC where possible. Direct CLI addresses require exactly 64 hexadecimal digits with a lowercase `0x` prefix, matching the existing address/alias configuration contract. Transaction hashes and cursors accept the shared RPC hex type's prefixed or unprefixed forms. All output uses canonical lowercase `0x` values.

Do not convert `NotImplemented` into an empty receipt or history page. Explain that the selected endpoint does not expose the required indexer capability and suggest using a full local devnode or compatible remote RPC.

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
  
- Resolve hosted native-token and wallet entrypoint metadata from the same validated descriptor used by existing signed flows.
  
- Keep transaction hashes and cursors as untrusted input; validate size and encoding before RPC.
  
- Treat event, return, abort, status-reason, and backend strings as untrusted display data. Do not interpret them as terminal markup or instructions.
  
- Do not claim a successful transfer before an `ExecStatus::Ok` receipt.
  
## Backend compatibility
No RPC schema change is required.

Expected behavior by endpoint capability:

| Capability | Transfer | `tx show` | `tx list` |
|---|---|---|---|
| Submit + chain + indexer | Full support | Full support | Full support |
| Submit + chain, no indexer | Submission may succeed but receipt polling/inspection is unavailable | `NotImplemented` or backend unavailable | `NotImplemented` or backend unavailable |
| Read-only indexer endpoint | No transfer | Committed hashes may be inspectable; pending/unknown status can be unavailable without scheduler access | History supported |
| RPC-only profile with no usable wallet metadata | Fails before signing | Supported if read methods exist | Supported if read methods exist |

The CLI should report the missing capability, not infer support from profile names such as `local` or `devnet`.
## Implementation shape
Keep the work primarily inside `crates/tools/cli` and reuse generated RPC clients and shared types.

Expected areas:

- parser, command names, and dispatch in `src/main.rs`;
  
- a `transfer` command module;
  
- a `tx` command module for show/list orchestration;
  
- shared HOG amount parsing/formatting and metadata resolution;
  
- shared broad address resolution instead of command-specific copies;
  
- existing account/wallet resolution and signing builders;
  
- existing `tx_utils` submission and receipt behavior;
  
- stable versioned JSON report types;
  
- command-specific errors and hints.
  

Avoid copying the airdrop transaction builder wholesale. Extract only the native-HOG construction pieces shared by airdrop, wallet prefunding, and transfer, while leaving their higher-level policies separate.

No gateway, RPC API, or protocol change is expected. Local end-to-end tests may need richer fake-indexer fixtures to exercise non-empty address history.
## Phasing
### Phase 1: transfer
- shared checked HOG amount parser and formatter;
  
- validated native-token metadata resolution;
  
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
  
2. Transfer uses the effective signer wallet, authoritative network metadata, exact amount conversion, and existing expiry/sign/submit/poll semantics.
  
3. Transfer dry-run makes no submission and clearly returns a prepared, submit-capable signed transaction.
  
4. Live transfer exits successfully only after an `ExecStatus::Ok` receipt and retains receipt context on execution failure.
  
5. `tx show` accurately represents pending, committed, invalid, recent-failure, unknown, future-status, and committed-without-receipt states.
  
6. `tx list` resolves supported address forms, returns one newest-first page, and preserves cursor and indexer retention metadata.
  
7. No inspection command uses the optional dev-detail method or claims fields unavailable from public RPC.
  
8. Root-invocation-only history semantics are visible in help and documentation.
  
9. Unsupported endpoint capabilities produce actionable errors rather than empty results.
  
10. Human and JSON output remain deterministic, versioned, and cleanly separated from status/error streams.
  
## Test plan
### Unit tests
- valid HOG decimal forms, minimum unit, canonical formatting, excess precision, signs, exponent notation, zero, and overflow;
  
- recipient/address precedence and collision behavior;
  
- signer-wallet ownership validation;
  
- hosted metadata validation and local fallback behavior;
  
- transaction hash, cursor, and limit parsing;
  
- `tx show` state composition, including committed status without a receipt;
  
- stable JSON serialization for success, semantic statuses, and execution failure.
  
### Binary integration tests
Use mock RPC fixtures to cover:

- transfer dry-run with no submit request;
  
- live transfer success and exact signed inner HOG invocation;
  
- committed revert/deterministic failure, invalid status, timeout, and backend failure;
  
- profile and RPC overrides;
  
- every public transaction status;
  
- receipt events, return data, and abort data;
  
- empty, non-empty, and continued history pages;
  
- malformed input and `NotImplemented` responses;
  
- human output and JSON stream discipline.
  
### End-to-end tests
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
  
- transfer source-wallet selection once shared across all write commands;
  
- `ALL`, batch transfers, arbitrary token standards, and allowance flows;
  
- incoming asset history backed by event or token indexes;
  
- deep trace/replay and storage-delta inspection;
  
- automatic multi-page history traversal.
