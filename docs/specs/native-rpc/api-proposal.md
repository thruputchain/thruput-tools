# Native Thruput JSON-RPC API specification

**Agreed target contract, updated 18 October 2026.** This specifies the API to implement,
not current server behaviour. Implementation status is tracked in Linear.

Read the method map and conventions here, then the
[contract appendix](appendix/contract-details.md) for exact shapes, edge cases
and source references. [Linear](https://linear.app/grndhg/document/native-rpc-delivery-and-first-batch-review-79be0ac4d8c9) owns delivery, dependencies and readiness.

## Why change the surface now?

PQ and encoding changes already require a coordinated API break. Use it to
settle the public surface before announcement: native Thruput semantics, with
familiar names and calling patterns where they fit.

Submission, commitment and execution are separate. Simulation discards writes
and needs no footprint preparation. Gas meters work rather than burning tokens.
The native surface uses wire **14.0.0** and a matching SDK. Wire 13 is the
separate Base58 migration of the existing API.

## Complete method map

Current names below omit their `groundhog_` prefix. Proposed core names are bare
camelCase. Optional faucet and local services use `faucet_` and `dev_`. The
parameters column is the JSON-RPC `params` object; it has no `request` or `query`
wrapper. Types and defaults are defined in the [contract appendix](appendix/contract-details.md).

| Current suffix | Proposed method | Parameters | Result |
| --- | --- | --- | --- |
| `version` | `getVersionInfo` | `{}` | Wire version, configured methods, diagnostic build identity |
| `networkInfo` | `getNetworkInfo` | `{}` | `{ networkHash }` |
| `chainTip` | `getChainTip` | `{}` | `{ blockHeight, blockHash }` |
| `chainConfig` | `getChainConfig` | `{}` | Stated limits and optional effective execution config with provenance |
| `serverLimits` | `getServerLimits` | `{}` | Endpoint-enforced simulation and pagination bounds |
| `sendRawTransaction` | `sendTransaction` | `{ signedTransaction }` | `{ transactionHash, admission: "accepted" }` |
| `getTransactionStatus` | `getTransactionStatus` | `{ transactionHash }` | Current lifecycle observation |
| `getTransactionReceipt` | `getTransactionReceipt` | `{ transactionHash }` | Committed receipt or `null` |
| `view` | `simulateInvocation` | `{ address, entrypoint, calldata, caller?, assumedAuth?, gasLimit?, minStateBlockHeight? }` | `{ stateBlockHeight, output, events, gasUsed }` |
| `viewBatch` | `simulateInvocations` | `{ invocations, minStateBlockHeight? }` | `{ stateBlockHeight, results }` |
| `getStorageAt` | `getStorage` | `{ address, key }` | `{ stateBlockHeight, item }`; item can be `null` |
| `getBalance` | `getBalance` | `{ address }` | `{ address, stateBlockHeight, rawBalance }` |
| `getAddressAuthData` | `getRegisteredAuth` | `{ address }` | `{ address, stateBlockHeight, configuration }`; configuration can be `null` |
| `getEvents` | `getEvents` | `{ filters, start?, cursor?, limit? }` | Events, indexed bounds and nullable `nextCursor` |
| `getTransactionsByAddress` | `getTransactionsByRootAddress` | `{ address, start?, cursor?, limit? }` | Root-target transaction references, indexed bounds and nullable `nextCursor` |
| `getBlockByNumber` | `getBlockByHeight` | `{ blockHeight }` | Committed block metadata or `null` outside indexed coverage |
| `getTransactionsByBlock` | `getTransactionsByBlock` | `{ blockHeight, start?, cursor?, limit? }` | Exact-block transaction references, indexed bounds and nullable `nextCursor` |
| `requestAirdrop` | `faucet_requestAirdrop` | `{ recipient, clientNonce }` | Existing pending/succeeded funding-attempt semantics |
| `fundAccount` | `dev_fundAccount` | `{ recipient, rawAmount, clientNonce, faucet }` | Retained transaction hash and signed submission evidence |
| `pendingTransactionCount` | `dev_getPendingTransactionCount` | `{}` | `{ count }`; local implementation only |

The first 17 methods form the full application endpoint. Optional services and
partial developer endpoints advertise only configured, implemented methods.
Temporary outages do not change getVersionInfo.supportedMethods.

Transaction body/envelope reads, combined getTransaction, transaction preparation, fee
estimation, public health, account nonce and confirmation levels are out of
scope. Firehose remains separate. Existing SDK prepareTransaction stays: it
assembles a transaction, not a footprint or gas estimate.

## Shared wire conventions

- **Requests:** strict named objects for methods with parameters. Reject unknown
  fields and explicit null optional fields unless documented. For noarg methods,
  emit `{}` and accept omission or `[]`. Also accept noarg `null` as omission:
  the pinned transport collapses those before dispatch. Nonempty noarg
  objects/arrays are invalid.
- **Identifiers:** canonical case-sensitive base58 for typed addresses, keys,
  hashes and signatures. C/G StrKey conversion belongs in typed human-input
  helpers, not as an alternate RPC/provider wire format. Public keys, key hashes
  and derived account addresses remain distinct types.
- **Addresses:** an address identifies a contract instance, including pubkey
  wallet accounts; contract code is identified separately by its deployed
  contract hash and type. Thruput has no Stellar-style account/contract split.
- **Opaque bytes:** lowercase `0x` hex for signed XDR, calldata, return/abort
  buffers, storage keys, topics, event data and opaque cursors.
- **Numbers:** canonical decimal strings for integers 64 bits or wider; JSON
  numbers for smaller bounded integers. Heights are nonnegative signed-64 values.
- **Response evolution:** tolerate additional fields and preserve unfamiliar
  variants as unsupported data. Never turn an unknown outcome into success,
  absence or retry permission. New request features require a compatible minor
  version; do not silently discard a requested constraint.

Heights identify commitment (`blockHeight`), read snapshot (`stateBlockHeight`),
simulation freshness floor (`minStateBlockHeight`) or advisory failed attempt
(`attemptBlockHeight`). History bounds describe availability, not checkpoints.

## Submit, observe commitment, inspect execution

These are schematic examples. Angle-bracket values stand for correctly encoded
identifiers or XDR; they are not runnable fixtures.

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "sendTransaction",
  "params": { "signedTransaction": "0x<signed-XDR>" }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "transactionHash": "<base58-body-hash>",
    "admission": "accepted"
  }
}
```

The server submits already signed bytes. Success means authoritative admission,
including an already-pending duplicate, not commitment or execution success.

Poll `getTransactionStatus({ transactionHash })` for commitment, then retrieve
`getTransactionReceipt({ transactionHash })`:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": { "status": "committed", "blockHeight": "812" }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "transactionHash": "<base58-body-hash>",
    "blockHeight": "812",
    "execution": { "result": "return", "returnData": "0x" },
    "events": [],
    "gasUsed": "12700"
  }
}
```

Receipt and simulation share return/abort/fault labels. Return and abort require
their respective byte fields; `"0x"` is empty. Unknown outcomes remain unsupported.
Events and gas stay beside execution because they apply across outcomes.

A committed transaction can fail execution. Null after committed status can mean
receipt lag; lookup failure remains an error. Malformed receipt data fails the
lookup, with only independently validated hash/height retained as commitment
evidence. Later weaker observations cannot erase known commitment.

Existing waitForTransactionReceipt, assertTransactionSuccess and waitForExecution
keep the SDK flow simple; waitForCommitment adds an observe-only option. Receipt
waiting returns failed execution too, and the assertion checks it. Rust and TS
waiters preserve commitment through errors and timeouts. They never resubmit.

## Simulate an invocation

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "simulateInvocation",
  "params": {
    "address": "<base58-address>",
    "entrypoint": "0x0102030405060708",
    "calldata": "0x",
    "gasLimit": "500000",
    "minStateBlockHeight": "812",
    "assumedAuth": {
      "kind": "pubkeySignature",
      "pubkeyHash": "<base58-public-key-hash>",
      "accountNonce": "77"
    }
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "stateBlockHeight": "813",
    "output": { "result": "return", "returnData": "0x" },
    "events": [],
    "gasUsed": "12700"
  }
}
```

Simulation reads committed state H and executes in context H+1. Writes are
discarded. Assumed authorization is not signature validation. `accountNonce` is
account derivation input, not the transaction uniquifier nonce.

Batch invocations start independently at one snapshot, preserve input order and
cannot see each other's writes. Losing the snapshot fails the batch. Simulation
does not validate the signed envelope, attachments, replay protection or admission.

A submitted root gas budget reserves scheduling capacity against the root
account's free HOG. It is not a payment or burn; storage collateral is separate.
getBalance reports free native HOG excluding locked deposits. Simulate the actual
wallet-wrapped root where relevant and leave headroom for work outside simulation.

## History and pagination

Event and root-address history require exactly one of start or a nonempty cursor. Repeat
predicates on continuation; page size may change within the advertised bound.
Cursors bind the query and fix its upper watermark. Only nextCursor: null means
exhaustion; empty pages can have a continuation. Event scans cap examined
candidates, not database latency or cancellation time.

Topic positions are optional wildcards; supplied topics are raw 32-byte hex even
when they contain an address. Reuse typed event helpers. Incoming native
transfers can be queried through the token Transfer event and destination topic;
this is not complete balance history.

`getTransactionsByRootAddress({ address, start?, cursor?, limit? })`:
Returns transactions whose root invocation targets the supplied address, including
DefaultWallet account addresses. The root address is the transaction's sender:
authorization is checked against it, and replay protection is scoped to it.
Results exclude transactions where the address is only called internally or
receives a transfer. For incoming native transfers, query `getEvents` for the
token Transfer event.

Event order is height, decoded transaction hash bytes, then sequence number.
It is not causal order or base58 string order.

The SDK walker checkpoints the initial watermark W only after processing all
pages through exhaustion. Data errors or serving height below W fail the walk.
Cross-reset recovery and cancellation remain follow-ups.

## Read a committed block

`getBlockByHeight({ blockHeight })` returns committed metadata, including its
canonical hash, parent hash, protocol, timestamp and consensus commitments.
It does not return transaction bodies. Indexed coverage starts at height 1;
height 0 and heights beyond coverage return `null`. Backend failures are errors.

`getTransactionsByBlock` pages through one height in decoded transaction-hash
order, not execution order. Repeat `blockHeight` on every call. Start with
`start: { order: "ascending" }` (or `"descending"`); continue with a nonempty
`cursor` and no `start`. There is no range anchor for an exact-block query.
The default limit is 50; the maximum is the advertised transaction-page cap,
which cannot exceed 200. Only `nextCursor: null` ends the walk.

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "getTransactionsByBlock",
  "params": {
    "blockHeight": "812",
    "start": { "order": "ascending" },
    "limit": 50
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "transactions": [{ "transactionHash": "<base58-body-hash>", "blockHeight": "812" }],
    "nextCursor": null,
    "oldestIndexedBlockHeight": "1",
    "latestIndexedBlockHeight": "814"
  }
}
```

A cursor binds the height, committed block identity and direction. Malformed or
mismatched cursors fail explicitly. A replacement indexer behind the requested
block returns an availability error on continuation. An empty initial page does
not prove that a block exists; use `getBlockByHeight`. See the
[block contract](appendix/contract-details.md#committed-block-reads) for fields,
coverage and integrity rules.

## Errors and recovery

Use standard JSON-RPC errors with stable typed `data.kind`. Messages are
human diagnostics. Cause, current-attempt admission evidence and SDK recovery
policy are separate:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32051,
    "message": "Submission outcome unknown",
    "data": {
      "kind": "backendTimeout",
      "admission": "unknown",
      "transactionHash": "<base58-body-hash>"
    }
  }
}
```

Admission evidence describes this attempt. Later refusal cannot resolve an
earlier uncertain send, and a numeric code alone proves no admission outcome.
Parameter reason/path fields are optional and evidence-backed. See the
[error catalogue](appendix/contract-details.md#proposed-common-catalogue-and-optional-reason-enums)
for codes and the [admission rules](appendix/contract-details.md#admission-evidence)
for queue/deadline handling.

Retain approved signed bytes, body hash and expected network before dispatch.
Exact-byte recovery requires qualified duplicate handling, unchanged chain
context, remaining validity, no commitment and continuing permission. Expiry,
missing records and dropped status do not prove nonexecution; never automatically
refresh nonce/expiry or reuse consumed wallet approval.

Funding retains its current results with narrow evidence/error corrections. SDK
requestAirdrop makes one faucet_requestAirdrop call on hosted or local endpoints.
Pending with a hash starts read-only status/receipt waiting; succeeded completes
after validation. An error or pending without a hash stops progression with
retained evidence. Explicit retries retain delay hints and original identity;
a repeated nonce or unchanged URL does not prove durable idempotence.

## Specification and delivery

The API surface is agreed. Changes to methods, payloads or semantics must update
this specification and its [contract appendix](appendix/contract-details.md).
Delivery slices, owners, dependencies, implementation gaps and announcement
qualification are tracked in [Linear](https://linear.app/grndhg/document/native-rpc-delivery-and-first-batch-review-79be0ac4d8c9).
