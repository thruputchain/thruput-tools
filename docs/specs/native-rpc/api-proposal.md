# Proposal: native Thruput JSON-RPC API

**Draft for team review, 15 October 2026.** This describes a proposed API, not
current server behaviour. Implementation and Linear changes remain on hold.

Read this proposal first, then the [delivery plan](delivery-plan.md). The
[contract appendix](appendix/contract-details.md) has complete field shapes and
edge cases. The [evidence appendix](appendix/review-evidence.md) records source
checks, all 65 original finding dispositions and the follow-up review corrections.

## Why change the surface now?

The rename, encoding migration and PQ work already require coordinated changes
across the RPC, SDK and applications. We should use that break to settle the
public API before announcement.

The proposal is to make the API feel native to Thruput. Keep familiar calling
patterns and names where their meaning fits. Make differences explicit:
submission is separate from commitment and execution; simulation runs an
invocation without committing writes; gas meters work rather than charging or
burning tokens. There is no footprint preparation step.

There are no external devnet users or state to preserve. The delivery path is one
matching artifact set, a fresh devnet, qualification and announcement. The
proposal therefore targets one new wire major and a matching native SDK.

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
| `view` | `simulateInvocation` | `{ contract, entrypoint, calldata, caller?, assumedAuth?, gasLimit?, minStateBlockHeight? }` | `{ stateBlockHeight, output, events, gasUsed }` |
| `viewBatch` | `simulateInvocations` | `{ invocations, minStateBlockHeight? }` | `{ stateBlockHeight, results }` |
| `getStorageAt` | `getStorage` | `{ address, key }` | `{ stateBlockHeight, item }`; item can be `null` |
| `getBalance` | `getBalance` | `{ address }` | `{ address, stateBlockHeight, rawBalance }` |
| `getAddressAuthData` | `getRegisteredAuth` | `{ address }` | `{ address, stateBlockHeight, configuration }`; configuration can be `null` |
| `getEvents` | `getEvents` | `{ filters, start?, cursor?, limit? }` | Events, indexed bounds and nullable `nextCursor` |
| `getTransactionsByAddress` | `getTransactionsByRootContract` | `{ contract, start?, cursor?, limit? }` | Root-target transaction references, indexed bounds and nullable `nextCursor` |
| `requestAirdrop` | `faucet_requestAirdrop` | `{ recipient, clientNonce }` | Existing pending/succeeded funding-attempt semantics |
| `fundAccount` | `dev_fundAccount` | `{ recipient, rawAmount, clientNonce, faucet }` | Retained transaction hash and signed submission evidence |
| `pendingTransactionCount` | `dev_getPendingTransactionCount` | `{}` | `{ count }`; local implementation only |
| `getBlockByNumber` | Deferred | None | No implemented public backend to advertise |

The first 15 methods form the full application endpoint. Optional services appear
only when configured and implemented. Explicit partial developer endpoints may
serve fewer methods. A temporary backend outage does not change discovery.
`getVersionInfo.supportedMethods` comes from actual configured registration.

No block/detail stubs, combined `getTransaction`, transaction-preparation RPC,
fee estimator, public health dump, account nonce RPC or confirmation levels are
added. Firehose remains a separate decision. Existing SDK `prepareTransaction`
stays: it assembles a transaction, not a footprint or gas estimate.

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
- **Opaque bytes:** lowercase `0x` hex for signed XDR, calldata, return/abort
  buffers, storage keys, topics, event data and opaque cursors.
- **Numbers:** canonical decimal strings for integers 64 bits or wider; JSON
  numbers for smaller bounded integers. Heights are nonnegative signed-64 values.
- **Response evolution:** tolerate additional fields and preserve unfamiliar
  variants as unsupported data. Never turn an unknown outcome into success,
  absence or retry permission. New request features require a compatible minor
  version; do not silently discard a requested constraint.

Height fields describe their role: `blockHeight` for commitment,
`stateBlockHeight` for the snapshot read, `minStateBlockHeight` for a simulation
freshness floor, and `attemptBlockHeight` for an advisory failed attempt. History
pages report serving bounds, not application checkpoints.

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

The server broadcasts already signed bytes. It never signs or asks a wallet to
sign. Success means authoritative admission, including an already-pending
duplicate. It does not mean commitment or successful execution.

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

Current execution labels become `return`, `abort` and `fault`, shared with
simulation. `returnData` and `abortData` are required for their respective
variants; `"0x"` means empty bytes. The fresh chain needs no missing-buffer
compatibility case. Unknown outcomes remain distinguishable. Gas and events
stay beside `execution` because they apply across outcomes.

A committed transaction can fail execution. A null receipt after committed
status can mean receipt lag; an indexer failure must remain an error. Keep known
commitment through weaker later observations. Malformed events or inconsistent
execution data fail the lookup rather than producing a successful partial
receipt. Receipt-error evidence contains only validated transaction hash and
block height. The shared adapter must check the backend log hash against the
requested hash before returning either a receipt or commitment evidence.

The SDK preserves the ordinary flow through existing `waitForTransactionReceipt`,
`assertTransactionSuccess` and `waitForExecution`, with an observe-only
`waitForCommitment` helper. Receipt waiting returns failed receipts; the assertion
checks execution. `KnownCommitment` carries network hash, transaction hash and
block height. The shared Rust waiter must preserve commitment through weaker
statuses, lookup failures and timeouts too. Waits never sign or resubmit.

## Simulate an invocation

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "simulateInvocation",
  "params": {
    "contract": "<base58-contract>",
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

Batch invocations start independently at one snapshot and cannot see each
other's writes. Results preserve input order; losing the shared snapshot fails
the whole request. Simulation does not check the complete signed envelope,
attachments, replay protection or eventual admission.

Gas meters execution. For a submitted transaction, the declared root gas budget
reserves scheduling capacity against the root account's free HOG. It is not a
token payment or burn. Storage collateral is separate.
`getBalance` reports free native HOG excluding locked deposits. Simulate the
actual wallet-wrapped root when relevant, and leave headroom for work outside
invocation simulation.

## History and pagination

Both history methods require exactly one of `start` or a nonempty `cursor`.
Repeat predicates on continuation; page size may change within the advertised
bound. Cursors bind the method and query and fix the walk's upper watermark.
`nextCursor: null` means exhaustion. Empty pages with a cursor are not terminal.
A bounded event scan advances past its last validated candidate, including
nonmatches. This bounds candidate count, not database latency or cancellation.

Topic positions are optional wildcards; supplied topics are raw 32-byte hex even
when they contain an address. Reuse typed event helpers. Incoming native
transfers can be queried through the token Transfer event and destination topic;
this is not complete balance history.

`getTransactionsByRootContract` covers the root invocation target only, not
nested calls or every address touched. Event order is height, decoded transaction
hash bytes, then sequence number. It is not causal order or base58 string order.

The SDK walker retains the initial upper watermark W and checkpoints it only
after processing terminal completion. Iterator/data failures and a serving
watermark that falls below W must fail, not look like exhaustion. Durable
cross-reset recovery and scan cancellation remain separate work.

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

`notAttempted`, `rejected` and `unknown` describe this attempt only. A later
refusal does not resolve an earlier uncertain send. Numeric codes alone never
establish admission. Preserve evidence through queue expiry and outer deadlines;
claim `notAttempted` only when later dispatch is ruled out. Structural parameter
errors may carry only `kind: "invalidParams"`; optional reasons and paths must
come from reliable validation evidence, never parsed error messages. The [full catalogue](appendix/contract-details.md#proposed-common-catalogue-and-optional-reason-enums)
includes freshness `-32012`, unavailable history `-32013`, invalid backend data
`-32054`, funding stop `-32030` and unresolved funding `-32031`.

Retain approved signed bytes, body hash and expected network before dispatch.
Exact-byte recovery requires qualified runtime duplicate handling, unchanged
chain context, remaining validity, no known commitment and continuing caller
permission. Never automatically refresh expiry/nonce or replay a consumed wallet
approval. Expiry, missing records and dropped status do not prove nonexecution.

Funding keeps its existing results with narrow error/evidence corrections.
For this major, the hosted SDK helper makes one funding-service call and does
not automatically repeat it, including after a policy refusal or pending result.
Reconcile known hashes through reads; otherwise return retained unresolved
evidence. Preserve delay hints for explicit caller retries. A repeated nonce or
unchanged URL does not prove durable idempotence. Funding-service policy errors
do not carry transaction admission evidence.

## Questions for review

- Do the names and payloads describe what Thruput actually does, particularly
  signed submission, invocation simulation, registered auth and root history?
- Does the separate status/receipt API provide enough evidence while the SDK
  keeps ordinary application code simple?
- Are the strict parameter rules, canonical wire encodings and proposed error
  families worth settling in this major? Which would be costly to reverse?
- Are any proposed guarantees unsupported by the minimum backend work, or are
  we making an additive improvement unnecessarily block announcement?

Continue with the [delivery plan](delivery-plan.md) for scope, parallel work and
release dependencies. Detailed objections can cite the relevant
[contract section](appendix/contract-details.md) or
[finding disposition](appendix/review-evidence.md).
