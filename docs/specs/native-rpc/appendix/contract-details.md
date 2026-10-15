# Appendix: native RPC contract details

This appendix defines the target shapes and edge cases behind the
[API specification and complete method map](../api-proposal.md).
[Linear](https://linear.app/grndhg/document/native-rpc-delivery-and-first-batch-review-79be0ac4d8c9) owns implementation scope, dependencies and readiness.
These are agreed target semantics, not a claim of implemented behavior.
Rust types will generate the implementation schemas. The sketches below are
design notation, and angle-bracket examples are not executable fixtures.

## Decision register

| Decision | Selection |
| --- | --- |
| Method surface | Bare core names; optional faucet_/dev_ services; separate status and receipt. |
| Address naming | Simulation/event fields use address/addresses; history and its SDK helper use getTransactionsByRootAddress, including wallet accounts. |
| Parameters | Strict named objects. Noarg canonical {}; accept omission/[] and transport-normalized null. |
| Encoding | Canonical case-sensitive base58 identifiers; typed C/G human-input conversion; opaque bytes remain hex. |
| Execution | Nested receipt execution with sibling events/gas; required return/abort bytes; gas meters work. |
| Heights | Distinct commitment, state, freshness-floor and failed-attempt fields. |
| Discovery | Configured registration determines methods; all 15 core methods required for announcement. |
| Errors | Separate cause, attempt admission and recovery; optional evidence-backed reasons; raw-preserving readers. |
| Commitment | Validate log identity/height first; error observation contains only hash/height; Rust and TS retain commitment. |
| History | Start/cursor exclusivity, query binding and fixed watermark; fail closed; candidate cap/direct seek in scope. |
| Funding | One SDK faucet call, then read-only waiting for a pending hash. Durable recovery and richer outcomes follow. |

Keep existing SDK assertTransactionSuccess, waitForExecution and prepareTransaction
helpers. The latter assembles a transaction, not a footprint or gas estimate.
The chosen names describe supplied data: getChainTip has no block body,
simulateInvocation does not validate a complete signed envelope,
getTransactionsByRootAddress excludes internal calls and incoming transfers, and
getRegisteredAuth cannot recover a pubkey account's key.

## Shared JSON contract

Use JSON-RPC 2.0 with one named params object. No request/query wrapper. Nested objects represent real structure, such as assumed authorization or pagination start.

- Canonical no-argument call: `params: {}`. Accept omitted `params`, `[]` and noarg `null` normalized to omission. Reject nonempty objects/arrays. For parameterized methods require a named object; omitted/null/array params are invalid. This narrow noarg exception is deliberate: jsonrpsee 0.25.1 collapses absent and null before dispatch. Do not fork the transport or add a second envelope parser for this distinction.
- Optional request fields are omitted. Reject explicit null unless a field specifically allows it. Reject unknown request properties at every boundary, including tagged-union variants.
- Outputs tolerate additional fields. Readers preserve or surface unknown union variants as unsupported data; they must not infer success, absence or permission to replace a transaction.
- Use base58 for typed addresses, keys, hashes and signatures. Preserve hex for signed XDR, calldata, return/abort buffers, raw storage keys, topics, event data and opaque cursors. RPC/provider typed identifiers accept canonical base58 only, not C/G wire aliases. Explicit typed human-input SDK/CLI/wallet helpers may convert C addresses or G keys before serialization; S is only explicit secret import/export. No legacy hex fallback for identifiers.
- An address identifies a contract instance, including pubkey wallet accounts; contract code is identified separately by its deployed contract hash and type. Thruput has no Stellar-style account/contract split. See the shared [address type](https://github.com/thruputchain/thruput-protocol/blob/47f88b234927cb20909e0af7185db9b5a977b613/types.x#L133-L136), [pubkey address derivation](https://github.com/thruputchain/thruput-protocol/blob/47f88b234927cb20909e0af7185db9b5a977b613/preimage.x#L19-L23) and [deployed code metadata](https://github.com/thruputchain/thruput-protocol/blob/47f88b234927cb20909e0af7185db9b5a977b613/contract.x#L236-L272).
- All 64-bit-or-wider integers are canonical decimal strings, including small values. Smaller bounded integers are JSON numbers. Domain bounds remain enforced; a string is not permission to accept arbitrary precision everywhere.
- `transactionHash` always means the canonical hash of the unsigned transaction body. It is neither a signature nor an identity for the exact signed envelope.
- `blockHeight` identifies a committed block; `stateBlockHeight` identifies the committed snapshot read. `attemptBlockHeight` identifies a failed proposal attempt. Separate reads can return different heights.
- A successful JSON-RPC transport response does not mean successful transaction execution. Execution outcomes belong in results; inability to answer belongs in errors.

```ts
type U32 = number;
type I32 = number;
type U64 = string;           // canonical unsigned decimal, range checked
type Amount = string;        // canonical decimal, bounded by its protocol field
type Address = string;       // typed 32-byte base58
type Hash = string;          // typed base58, length checked by its field
type Hex = string;           // 0x-prefixed lowercase, complete bytes
type Cursor = Hex;           // opaque, nonempty bytes when used to continue
type Height = string;        // canonical nonnegative decimal, maximum 2^63 - 1
```

Strict request DTOs own omission/null/unknown-field validation. Generate schemas from those DTOs and preserve gateway/local/client/schema parity. Parameterized methods declare `paramStructure: "by-name"` plus whole-object `x-params-schema`; document noarg normalizations separately and validate the selected OpenRPC meta-schema. See [OpenRPC](https://spec.open-rpc.org/) and the [existing generator](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/open-rpc-spec/src/lib.rs).

Use distinct SDK semantic types/constructors for keys, public-key hashes and derived addresses. Never implicitly turn a G/public key into an account address. Valid32-byte text cannot always reveal a mistaken semantic type. The body hash excludes network identity; signatures bind it. Records use verified networkHash plus transactionHash and explicit reset invalidation, even if networkHash is reused.

### Height fields

Use `blockHeight` for block identity and committed inclusion, `stateBlockHeight` for the snapshot behind a state read or simulation, and `attemptBlockHeight` for an advisory failed proposal attempt. Use `minStateBlockHeight` for the simulation freshness floor. Page bounds keep the explicit `oldestIndexedBlockHeight` and `latestIndexedBlockHeight` names; they are serving bounds, not processed checkpoints. Inside `execution.observedTip`, use the same `{ blockHeight, blockHash }` shape as the chain tip.

A uniform `height` would be shorter, but would leave its meaning to the enclosing method or object. The proposed explicit fields preserve the distinction when values reach SDK objects, logs or application state. Do not add a generic `context` wrapper or redundant derived heights. For example, a receipt at block 812 and balance read at state block 813 are compatible observations, not a promise that the balance includes only that receipt. Simulation reads state H and runs in execution context H+1; report the actual state height H.

All public heights remain canonical decimal strings bounded to nonnegative signed-64 values. No field name creates an exact-height selector, cross-call snapshot guarantee or block-hash provenance the backend does not supply.

## Submit and observe a transaction

### Submission

`sendTransaction({ signedTransaction })` returns
`{ transactionHash, admission: "accepted" }` only after authoritative admission,
including an already-pending duplicate. This does not imply continued pool
presence, commitment or execution success. The [API proposal](../api-proposal.md#submit-observe-commitment-inspect-execution)
shows the envelopes. Client evidence and redispatch rules are defined under
[SDK recovery](#sdk-recovery-policy).

### Status

```ts
// getTransactionStatus({ transactionHash: Hash })
type TransactionStatus =
  | { status: "pending" }
  | { status: "committed"; blockHeight: Height }
  | { status: "retry"; error: SchedulerError }
  | { status: "dropped"; error: SchedulerError }
  | { status: "recentFailure"; attemptBlockHeight: Height; cause: FailureCause }
  | { status: "unknown" }
  | { status: "other" };

type SchedulerError = {
  source: "auth" | "scheduler" | "executor" | "other";
  code: I32;
};

type FailureCause =
  | { kind: "dbConflict"; conflict: {
      address: Address; key: Hex; code: ConflictCode;
    } }
  | { kind: "contractRejected"; contractIndex: U32; code: ContractRejectCode }
  | { kind: "other" };
```

Keep the current typed `ConflictCode` and `ContractRejectCode` vocabulary. Preserve raw scheduler source/code. Do not reduce these diagnostics to a generic failure string. [Owning types](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/transaction.rs).

Indexed commitment wins. See [shared status precedence](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/status.rs#L39). If the indexer fails, scheduler commitment may still answer; pending may report provisional presence without proving no earlier commitment. If the indexer fails and the scheduler returns retry/dropped/recent diagnostic/miss, propagate the read failure. Never hide it as a negative lifecycle result. `unknown` requires successful retained-source misses. Advisory recentFailure is not final failure. Preserve raw scheduler diagnostics, and treat other/unfamiliar tags as unsupported.

Commitment is final under Thruput consensus; no confirmation-depth polling is needed. Receipt null, retry, dropped, unknown and passed expiry do not prove historical nonexecution. Known client commitment cannot be downgraded. An authoritative indexedThrough/expiredUncommitted proof needs complete retained coverage and race semantics; defer it.

### Receipt

```ts
// getTransactionReceipt({ transactionHash: Hash })
type ReceiptResult = TransactionReceipt | null;

type TransactionReceipt = {
  transactionHash: Hash;
  blockHeight: Height;
  execution: ReceiptExecution;
  events: EmittedEvent[];
  gasUsed: U64;
};

type ReceiptExecution =
  | { result: "return"; returnData: Hex }
  | { result: "abort"; abortData: Hex }
  | { result: "fault" }
  | { result: "nonDeterministicError" }
  | { result: "failedGasSponsorship" }
  | { result: "other" };
```

Receipt execution uses return/abort/fault labels shared with simulation. Require returnData/abortData for their respective variants; `"0x"` is empty. Missing bytes fail validation. Events and gasUsed remain receipt-level fields.

Validate supported ExecResult/root-output combinations before attaching a buffer. Root Return alone cannot establish transaction success: commit can fail afterward, so failed ExecResult does not necessarily imply Fault root output. Contradictions return backendDataInvalid/inconsistentResult. Remove manufactured fault.kind and unproduced revert messages; do not infer out-of-gas. See the [root-output union](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/transaction.x#L88-L102) and [commit path](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/runtime/syscall_handler.cc#L537-L662).

Malformed events fail the receipt lookup with backendDataInvalid, not a successful partial receipt. Preserve only independently trustworthy observation evidence in the error:

```ts
type ReceiptObservation = {
  transactionHash: Hash;
  blockHeight: Height;
};
type BackendDataInvalidData = {
  kind: "backendDataInvalid";
  reason?: "invalidEncoding" | "invalidCardinality" | "unsupportedValue" | "inconsistentResult";
  observation?: ReceiptObservation; // receipt reads only; never fabricated from failed decoding
};
```

Validate `log.tx_hash == tx_hash_to_wire(tx_hash)` and a nonnegative height within the wire Height range before parsing events or checking execution consistency. Failure returns backendDataInvalid/inconsistentResult without observation. Only later parsing/consistency failures may attach the validated hash/height. The [current adapter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/receipt.rs#L29-L48) echoes the request hash, so client equality checks alone are insufficient. Clients validate observation identity/network context and retain known commitment. Invalid simulation events fail the whole single/batch result.

Gas measures work, not HOG payments or burns. The root gas budget reserves scheduling capacity against its free HOG, so a funded root is still required. Storage collateral is separate. SDK onboarding must explain this distinction; there is no gas-price conversion or fee-estimation RPC.

`null` means this successful lookup did not produce a committed receipt. It does not say pending, never submitted or definitively uncommitted. A `committed` status followed by a null receipt is valid. An indexer outage must not become null. A receipt includes execution results and events, not the transaction body, detached key or signatures.

The last historical outcomes need careful reader support. The protocol retires `failedGasSponsorship` and excludes `nonDeterministicError` from valid proposals. Preserve existing decoding without presenting them as ordinary current committed results. Investigate unexpected emission; do not invent gas sponsorship workflows from the enum. [Execution protocol](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/exec_result.x).

### SDK receipt waiting

Separate status and receipt reads keep polling small. SDK composition supplies
the ordinary flow:

- `waitForCommitment` returns `{ networkHash, transactionHash, blockHeight }`.
- `waitForTransactionReceipt` returns any committed receipt, including failed
  execution, and accepts optional knownCommitment in the same verified context.
- `assertTransactionSuccess` throws with the full failed receipt;
  `waitForExecution` composes waiting and assertion. Unknown outcomes produce a
  typed unsupported result with raw data. Waits never sign or submit.

Retain the 30-second default timeout and configurable 250 ms polling interval,
with bounded jittered backoff for transient read failures and caller cancellation.
Validate supplied commitment hash/network; require its height. Once commitment
is supplied or observed, later null/Unknown/Retry/Dropped cannot replace it.
Retry/Dropped must not end receipt waiting early. Status lookup errors preserve
commitment; receipt lookup failures and timeouts carry it in their errors. An
older RecentFailure cannot win at timeout. Reset-generation invalidation is
explicit; networkHash is not an instance identifier.

Apply these rules in TS and the [shared Rust waiter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-client/src/receipt.rs#L155-L248),
including CLI, faucet, simulator/testkit and embeddable callers. Test
Committed→Unknown, Committed→Retry/Dropped, Committed→lookup failure and
RecentFailure→Committed→timeout. Poll ordering remains a performance choice;
no global transaction cache is required.

Compare submission responses with the locally computed body hash and receipts
with the requested hash. A mismatch preserves the original identity and evidence
and fails validation. Missing required return bytes are invalid response data;
ABI decoding failure of present bytes is a separate application outcome. Neither
permits repeating a business action.

Rust and TS readers preserve unknown tags and raw payloads as unsupported data.
Test unknown variants containing nested fields. Rust non_exhaustive does not
change decoding; internally tagged serde(other) accepts extra fields but discards
tag/payload. Unknown backend XDR values need a supported projection or
backendDataInvalid/unsupportedValue, never success, absence or retry permission.
See [Serde variant attributes](https://serde.rs/variant-attrs.html).

Keep byte-array and bigint SDK ergonomics with validated transport codecs.

## Invocation simulation

```ts
type Invocation = {
  address: Address;
  entrypoint: Hex;           // exactly 8 decoded bytes
  calldata: Hex;
  caller?: Address;
  assumedAuth?: AssumedAuth;
  gasLimit?: U64;
};

type AssumedAuth =
  | { kind: "pubkeySignature"; pubkeyHash: Hash; accountNonce: U64 }
  | { kind: "sourceAddress"; allowedKeys: U32; requiredWeight: U32 };

type InvocationOutput =
  | { result: "return"; returnData: Hex }
  | { result: "abort"; abortData: Hex }
  | { result: "fault" };

type InvocationResult = {
  output: InvocationOutput;
  events: EmittedEvent[];
  gasUsed: U64;
};

// simulateInvocation(Invocation & { minStateBlockHeight?: Height })
type SimulationResult = InvocationResult & { stateBlockHeight: Height };

// simulateInvocations({ invocations: Invocation[], minStateBlockHeight?: Height })
type SimulationResults = {
  stateBlockHeight: Height;
  results: InvocationResult[];
};
```

```json
{
  "jsonrpc": "2.0",
  "id": 2,
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
  "id": 2,
  "result": {
    "stateBlockHeight": "813",
    "output": { "result": "return", "returnData": "0x" },
    "events": [],
    "gasUsed": "12700"
  }
}
```

The snapshot is committed state H, executed with context H+1. Writes are discarded. `caller` changes the simulated message sender; `assumedAuth` supplies an authorization assumption. Neither proves that a real signer exists or validates a signature. Contract authorization checks still execute.

Omitted auth preserves the current unauthenticated default. Omitted caller creates no synthetic parent frame: `get_msg_sender` returns `IsRootTransaction`, not a zero address, self address or connected wallet. A supplied caller creates the simulated parent context. Omitted gas selects the advertised endpoint maximum; zero and above-maximum values are invalid. `minStateBlockHeight` is a fail-fast freshness floor, not a historical selector, wait or forced refresh. [Caller handling](https://github.com/thruputchain/hogd/blob/7ca6a9d1a4c9fd5ac61d0a66d7a3790aaf3ad067/hogd/hog_exec/runtime/view/view.cc#L55).

Batch results preserve input order. Every invocation starts at the same snapshot and cannot see another invocation's writes. A return, abort or fault is a per-invocation result. Failure to obtain/retain the shared snapshot is an RPC error for the whole request. No automatic chunking, since chunking loses the common-snapshot guarantee. Ordinary JSON-RPC batching makes no snapshot guarantee.

Simulation is optional preparation for the application. It is not a required footprint/auth assembly step. It does not validate the complete transaction envelope, account signatures, expiry, replay protection, deployment attachments or eventual admission. Its gas figure is not a fee quote or a guarantee of real execution success. Attached-contract validation can add gas beyond invocation simulation, and transaction limits need headroom. [Current simulation contract](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-api/src/lib.rs#L176).

Use `accountNonce` for pubkey-account derivation, not the transaction uniquifier nonce. Simulate the exact wallet-wrapped root with matching assumed authorization; root simulation alone does not price attachments or wallet overhead. Keep SDK `prepareTransaction`: it assembles body/nonce/expiry, not simulation, gas sizing, authorization or footprints.

The selected `stateBlockHeight`, `minStateBlockHeight` and `assumedAuth` fields cost some typing but keep their different meanings visible in the request.

## State and registered authorization

```ts
// getStorage({ address: Address, key: Hex })
type StorageResult = { stateBlockHeight: Height; item: StorageItem | null };

type StorageItem =
  | { kind: "nonnegativeInt"; value: Amount }
  | { kind: "bytestring"; bytes: Hex }
  | { kind: "authData" }
  | { kind: "retrocausalQueue" }
  | { kind: "replayCacheEntry" }
  | { kind: "deployedContract"; hash: Hash; size: U32; contractType: I32 }
  | { kind: "unknown"; typeId: I32 };

// getBalance({ address: Address })
type BalanceResult = {
  address: Address;
  stateBlockHeight: Height;
  rawBalance: Amount;
};

// getRegisteredAuth({ address: Address })
type RegisteredAuthResult = {
  address: Address;
  stateBlockHeight: Height;
  configuration: null | {
    signers: { keyId: U32; publicKey: PublicKey; maxWeight: U32 }[];
    requiredWeight: U32;
  };
};

type PublicKey =
  | { scheme: "ed25519"; key: string }
  | { scheme: "unknown"; schemeId: I32 };
```

Keep the typed storage projection. `key` is the existing serialized storage-key representation, not an EVM slot. Storage null means no item at that snapshot. Native balance excludes locked storage collateral and is in indivisible HOG units, with 10,000,000 units per HOG; the current raw balance type remains bounded by the native storage model. Auth/config absence is not a missing balance or a nonexistent account.

For registered auth, signer slot IDs can have gaps. Keep their weights, threshold and numeric IDs. Do not normalize the list into positional IDs. `configuration: null` means no registered signer configuration; ordinary Ed25519 and PQ pubkey accounts can have this result. The method cannot recover their key or infer their scheme from an address.

Select only the implemented Ed25519 registered-key projection (32 decoded bytes) and the existing unknown scheme ID. Do not predeclare an emitted ML-DSA registered-key variant. PQ pubkey transactions remain required, but registered PQ is separate work. A future known scheme needs real capability and qualified tolerant readers before additive introduction. Signatures remain inside the submitted XDR envelope. Unknown key schemes are unsupported by a signer until explicitly implemented. [Auth projection](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/address_auth.rs).

Do not add fake historical selectors, pending-state reads, or a cross-read consistency guarantee. Extending freshness floors to these three reads is useful additive work, but it requires real enforcement rather than silently ignored parameters.

## Events and root address history

Initial and continuation requests are distinct. Repeat query predicates and use `nextCursor: null` for exhaustion.

```ts
type HeightAnchor = { kind: "atHeight"; blockHeight: Height };
type Start =
  | { order: "ascending"; anchor: { kind: "atBeginning" } | HeightAnchor }
  | { order: "descending"; anchor: { kind: "atEnd" } | HeightAnchor };

type PageRequest =
  | { start: Start; cursor?: never; limit?: U32 }
  | { cursor: Cursor; start?: never; limit?: U32 };

type TopicFilter = { topic0?: Hex[]; topic1?: Hex[]; topic2?: Hex[] };
type EventFilter = { addresses: Address[]; topics?: TopicFilter };
type EventQuery = PageRequest & { filters: EventFilter[] };
type RootAddressQuery = PageRequest & { address: Address };

type EmittedEvent = { address: Address; topics: Hex[]; data: Hex };
type IdentifiedEvent = {
  transactionHash: Hash;
  blockHeight: Height;
  sequenceNumber: U32;
  event: EmittedEvent;
};
type PageBounds = {
  oldestIndexedBlockHeight: Height;
  latestIndexedBlockHeight: Height;
};
type EventPage = PageBounds & {
  events: IdentifiedEvent[];
  nextCursor: Cursor | null;
};
type TransactionRefPage = PageBounds & {
  transactions: { transactionHash: Hash; blockHeight: Height }[];
  nextCursor: Cursor | null;
};
```

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "getEvents",
  "params": {
    "filters": [{
      "addresses": ["<base58-address>"],
      "topics": { "topic0": [], "topic1": [], "topic2": [] }
    }],
    "start": { "order": "ascending", "anchor": { "kind": "atHeight", "blockHeight": "800" } },
    "limit": 50
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "events": [],
    "nextCursor": "0x<opaque-continuation>",
    "oldestIndexedBlockHeight": "0",
    "latestIndexedBlockHeight": "814"
  }
}
```

Continuation repeats filters (or the root-history address), cursor and optional limit; it omits start. Require exactly one of start or a nonempty cursor. Default limit is 50; reject zero or values above the advertised cap. Page size may change. Bind the method and canonical decoded query, excluding page size. Preserve ordered filter/address/topic arrays; reordered lists may be rejected as queryMismatch. Object property order and equivalent wildcard forms do not change identity. Direction and upper watermark remain cursor-owned.

Retain current topic semantics: OR across filters; address membership AND topic conditions within each filter; OR within each topic position; AND across positions. An omitted topics object, omitted position or empty topic list is a wildcard. Normalize these equivalent forms before query binding; null is invalid. Each specified topic is exactly 32 bytes; there are at most three positions. Keep nonempty address lists and the current filter/value bounds. Do not interpret an empty address list as a full-chain scan.

An explicit `atHeight` anchor is inclusive when available. `atBeginning` is valid only ascending; `atEnd` only descending. Reject the opposite combinations as InvalidParams, rather than preserving today's surprising empty-page behavior. These boundaries refer to the retained serving range. An ascending explicit anchor beyond the available tip exhausts immediately; descending starts at the available tip. An explicit height below retained history should report unavailable history instead of silently clamping or exhausting; this requires the backend outcome described below.

The cursor fixes the upper bound and does not follow new blocks. Page bounds describe serving availability, not completed checkpoints. Continue through empty pages while nextCursor is non-null. The current oldest height is zero; that does not establish future pruning support.

An event's identity within a chain instance is `(transactionHash, sequenceNumber)`. For an ascending walk, the initial response's `latestIndexedBlockHeight` is the fixed upper watermark W encoded in that walk's continuation; preserve this existing backend relationship as an explicit contract. Retain W separately from later page bounds. Only after every page reaches `nextCursor: null` and all returned events have been processed may the application checkpoint through W. An empty completed range can checkpoint W too; an initial requested height above W does not advance a prior checkpoint. Restart the next walk inclusively at W and deduplicate the boundary. Persist application changes and its checkpoint together where needed. If any page fails or reports latestIndexedBlockHeight below W, do not mark the walk complete. The backend must reject continuation when current serving height is below cursor W as backendUnavailable, for both directions; it must not clamp W down and return false exhaustion. No cross-reset or pruning-proof durability is implied.

Receipt, event and scheduler retention differ; page bounds are not a global retention guarantee.

`getTransactionsByRootAddress({ address, start?, cursor?, limit? })`:

Returns transactions whose root invocation targets the supplied address, including DefaultWallet account addresses. The root address is the transaction's sender: authorization is checked against it, and replay protection is scoped to it. Results exclude transactions where the address is only called internally or receives a transfer. For incoming native transfers, query `getEvents` for the token Transfer event.

Sender semantics follow the [pubkey-address check](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/auth_claim/verify.cc#L260-L270), [root-address auth lookup](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_auth/auth_server.cc#L240-L280) and [address-scoped replay protection](https://github.com/thruputchain/thruput-protocol/blob/47f88b234927cb20909e0af7185db9b5a977b613/transaction.x#L108-L120).

The result contains transaction references, not bodies. Use `getTransactionsByRootAddress` for the planned sdk-ts helper too, with the same `address` parameter and scope. Keep `Root`: [Solana address history](https://solana.com/docs/rpc/http/getsignaturesforaddress) includes transactions referencing the address in accountKeys, and [Etherscan address history](https://info.etherscan.com/how-to-filter-transactions-of-an-address/) includes incoming transactions. Thruput indexes only the root target, as defined by the [protocol query](https://github.com/thruputchain/thruput-protocol/blob/47f88b234927cb20909e0af7185db9b5a977b613/indexer.x#L188-L200) and [index key](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L231-L240).

### History integrity

The full announced application profile requires both history methods. Partial developer endpoints may omit them; the full application profile cannot.

1. Invalid/malformed/unsupported cursors and predicate mismatch are typed protocol/indexer outcomes, mapped to invalidCursor/-32602. Binding includes method and canonical decoded query, excludes page size, remains backend-owned and fits the 256-byte cap. It prevents accidental mismatch, not malicious token forgery. A gateway must not decode cursor internals.
2. Check RocksDB iterator status, malformed in-range keys, every missing/unreadable/undecodable linked hydration row, out-of-range event position and malformed projection. Fail whole page with typed error and no successful cursor/exhaustion. Current atomic/no-pruning indexing gives no legitimate missing-linked-row exception. Cover events and root-address history, plus malformed receipt/single/batch simulation events.
3. Keep block/emitter consistency checks but turn query-triggered CHECK failures into typed errors rather than process termination. Retain appropriate ingestion invariants. Other queries must remain available when a query encounters invalid data.
4. Reject regressed serving watermark below cursor W; do not clamp continuation. This applies to ascending and descending walks, including restored or replaced indexers. SDK defensive walking must also refuse checkpoint advancement.
5. Report outsideRetention only with authoritative retained-range evidence. Current strict fresh origin requires epoch0, first height1 and contiguous later epochs, so oldest0 is genesis-inclusive and not a proven hidden gap. Do not infer non-genesis support or reset identity.

When start is absent, supply fixed valid legacy XDR order/anchor placeholders, such as Ascending/AtBeginning. Continuation uses cursor-owned order/position/W and ignores placeholders for query binding. Different valid placeholders must produce equivalent continuation results; omitting start alone needs no binary layout change.

Use a positive configured examined-candidate cap per event page. Resume exclusively after the last successfully validated visited position, including nonmatches, and emit a cursor only when candidates remain. Preserve query binding and W. Integrity/I/O errors fail the page rather than returning partial results or a checkpoint. Direct seeking must avoid rescanning positions before the continuation. The cap applies in both directions and must preserve exact exhaustion and result-limit behavior. Empty nonterminal pages are a target shape; the current uncapped loop does not normally emit them.

The candidate cap does not bound RocksDB latency, repeated whole-log decoding, initial fanout or post-timeout work. The API does not promise general time/byte budgets or cancellation. [Delivery and qualification](https://linear.app/grndhg/document/native-rpc-delivery-and-first-batch-review-79be0ac4d8c9) track those implementation limits.

SDK history walkers must freeze predicates and initial W, handle empty nonterminal pages, deduplicate event identities and emit completedThroughBlockHeight only after processing terminal completion with every serving bound at least W. Ascending order is `(blockHeight, decoded transactionHash bytes, sequenceNumber)`; descending reverses it. Root references use height/hash. This is not causal or base58 lexical order.

Incoming-native-transfer recipe: query the native-token address, canonical Transfer signature topic0 and destination address bytes as 32-byte hex topic2. Reuse generated typed topic helpers and test address-to-topic conversion. Generated signature leaves two indexed value positions. Transfer events are not full balance history. Freeze token event/error signature identity at 0.1 and `groundhog-erc20-if` for this release; a package 0.2/1 bump changes it even with the label pinned. Run the existing native/RV32 parity test with exact candidate pins. Independent event-version controls can follow. See [signature derivation](https://github.com/thruputchain/thruput-sdk-rs/blob/12337492533acfa45a475cabc5af88ffe456559d/crates/macros/src/derive_event.rs#L298-L313) and [native/RV32 parity coverage](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/contract_tests/native_rv32_token_event_parity_test.cc#L371-L419).

Sources: [indexer scan/cursor](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2090-L2214), [cursor seeking](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1861-L1885), [indexer protocol](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/indexer.x#L229-L259), and [RocksDB iterator errors](https://github.com/facebook/rocksdb/wiki/Iterator).

## Discovery and configuration

### Versions and supported methods

```ts
// getVersionInfo({})
type VersionInfo = {
  wireFormat: string;          // semver; supported-range compatibility gate
  supportedMethods: string[];  // configured implementation, not live health
  implementation: string;     // diagnostic only
  commitHash?: string;
  platformVersion?: string;
  releaseVersion?: string;
  bundleManifestDigest?: string;
};

// getNetworkInfo({})
type NetworkInfo = { networkHash: Hash };

// getChainTip({})
type ChainTip = { blockHeight: Height; blockHash: Hash };
```

Generate sorted, unique `supportedMethods` from actual configured registration, excluding stubs. Keep it stable through transient outages. A full-service endpoint advertises all 15 core methods; optional service methods appear only when configured. No backend fan-out is needed to answer discovery.

| Configured source | Methods |
| --- | --- |
| Validated static config | getVersionInfo, getNetworkInfo, getServerLimits |
| Scheduler | sendTransaction |
| DB | getChainTip, getChainConfig, getStorage, getBalance, getRegisteredAuth |
| Indexer | getTransactionReceipt |
| Indexer + scheduler | getTransactionStatus |
| Qualified indexer history | getEvents, getTransactionsByRootAddress |
| View backend | simulateInvocation, simulateInvocations |
| Local scheduler | dev_getPendingTransactionCount |
| Hosted faucet REST adapter | faucet_requestAirdrop |
| Local submit/indexer + validated signer/journal/funding config | dev_fundAccount and local faucet_requestAirdrop |

Finalize discovery after module merging/removal, before serving. Pinned jsonrpsee 0.25.1 supports remove_method; the chosen registration strategy must expose the final configured list. Configured outage returns an error; unconfigured planes are absent. Full service requires scheduler/DB/indexer/view. Startup must compare manifest protocol with linked codecs; an intro-token override alone cannot prove compatibility.

Keep `wireFormat` as the compatibility authority. Release/implementation fields remain diagnostic; validated bundle identity retains its existing producer-owned format and provenance. Do not blindly re-encode Git commit IDs or artifact identifiers as chain hashes. Omit unavailable diagnostic fields consistently, including commitHash.

Keep `getVersionInfo` bootstrap stable across later wire majors. The native SDK has no old-name diagnostic fallback. A missing wireFormat, including a Stellar response to the same method name, is a typed incompatible/non-Thruput endpoint error. Add fixtures for Stellar transaction/base64 and ledger/filter requests yielding invalidParams; valid noarg {} remains valid.

Cache discovery per client/transport instance; validate new instances and expose invalidation for mutable transports. Revalidate incompatible responses, required-method -32601 and network mismatch. Do not replay mutations after refresh or mix incompatible contracts behind one endpoint. Check wire range, required methods and caller/provider expected network before action, including before signing composed flows.

App proxies expose getVersionInfo/getNetworkInfo, advertise upstream support intersected with policy, accept named objects, preserve bounded typed errors and return -32601 for filtered methods. HTTP authentication/rate limiting is separate. Static SDK capabilities express expectations; unavailable optional methods fail locally. Test [Counter](https://github.com/thruputchain/thruput-examples/blob/c8ba8730d0d06e027dc939afb5974e32992f264f/apps/counter/server/http.ts), [swap](https://github.com/thruputchain/thruput-examples/blob/c8ba8730d0d06e027dc939afb5974e32992f264f/apps/swap/server/http.ts) and [token-launchpad](https://github.com/thruputchain/thruput-examples/blob/c8ba8730d0d06e027dc939afb5974e32992f264f/apps/token-launchpad/server/http.ts) independently, preserving their write policies. Shared helpers are optional.

New SDK versions target the native API only. New output variants are minor additions only after supported readers demonstrate safe raw/unsupported fallback; otherwise they require a major. The planning target is `>=13.0.0 <14.0.0`, conditional on the coordinated major and demonstrated minor-version reader compatibility. If that major already identifies a different published contract, choose the next coordinated major. Diagnostic implementation/release versions never select codecs. A new optional request field or enum variant is not automatically supported by an older server in the same major: gate it by observed wire minor or raise the SDK's minimum required minor. If the caller explicitly requests an unsupported feature, fail locally as unsupported; never silently drop that constraint. Test new-client/old-server and old-client/new-server directions, including strict unknown-field rejection; `supportedMethods` alone does not negotiate parameter support.

The public `networkHash` binds signing identity. It does not currently prove a particular reset/instance or provide a genesis descriptor. Do not add invented `genesisHash`, `chainInstanceId`, protocol version or native network label. A required verified instance identifier would be useful, but needs a runtime/bootstrap contract and explicit reset behavior.

The five information methods have distinct sources, cache rules and failures. SDK composition can reduce calling overhead without coupling discovery to backend availability.

### Chain rules and storage costs

```ts
// getChainConfig({})
type ChainConfig = {
  stated: ConfigLimits;
  execution?: {
    observedTip: ChainTip;
    configHash: Hash;
    effectiveLimits: ConfigLimits;
    storageCosts: StorageCosts;
  };
};

type ConfigLimits = {
  maxSha256Buffers?: U32;
  maxCrossInvokeDepth?: U32;
  maxNewContractsPerTx?: U32;
  maxBytestringLen?: U32;
  maxContractSizeBytes?: U32;
  maxSavepointDepth?: U32;
  maxAuthTokenDepth?: U32;
  maxRv32CodeBytes?: U32;
  maxRv32MemoryBytes?: U32;
};

type StorageCosts = {
  baseDataItemCost: Amount;
  bytestringCostPerByte: Amount;
  baseContractDeploymentCost: Amount;
  contractDeploymentCostPerByte: Amount;
  authPerKeyCost: Amount;
  rcqCostPerEntry: Amount;
  replayCacheCostPerEntry: Amount;
  balanceCost: Amount;
};
```

Select grouping under `stated` to distinguish explicit configuration from effective runtime limits. The Rust representation already separates these; this changes the flattened JSON projection, not the runtime producer.

Preserve `execution` provenance. Its limits/prices are read together with `observedTip`, may come from an allowed recent complete cached reading, and apply to execution following that tip. They are not a guarantee for later inclusion. Missing values mean unspecified/unavailable, not zero or unlimited. Do not replace a failed refresh with expired pricing. Storage amounts are native-HOG base units; collateral is distinct from gas capacity. [Current config types](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/network_config.rs).

### Endpoint limits

```ts
// getServerLimits({})
type ServerLimits = {
  maxSimulationGas: U64;
  maxSimulationCalldataBytes: U64;
  maxEventsPerPage: U32;
  maxTransactionsPerPage: U32;
  maxCursorBytes: U64;
  simulationBatch?: {
    maxInvocations: U32;
    maxCalldataBytes: U32;
    maxGas: U64;
    maxBackendResponseBytes: U32;
    timeoutMs: U64;
  };
};
```

Omit `simulationBatch` when batch simulation is unconfigured; temporary backend health does not change this metadata. `supportedMethods` remains the capability authority. Rename view vocabulary with the methods. Preserve current integer widths. Calldata and cursor limits count decoded bytes. Batch calldata and gas are aggregate budgets. The batch response bound applies to encoded backend data before JSON encoding; the new name makes that layer explicit. Existing transport limits still apply and may produce framework errors; this result does not claim to enumerate them.

Defer new transport-limit metadata. A later projection must distinguish JSON message/batch limits from decoded transaction limits and account for tighter ingress limits before claiming an end-to-end maximum. Do not expose assumed library defaults. Callers must handle actual rejections even after discovery.

Do not add connection counts, private pool sizes or a universal requests-per-second promise. Full PQ-envelope byte/memory and aggregate submission budgets still need measurement and enforcement. [Current limits](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/server_limits.rs).

After commitment at H, state-read callers compare returned `stateBlockHeight` with H and retry within their budget if behind. General state freshness parameters remain follow-up; no read silently inherits a previous receipt's height.

Keep public health out of this release. Operator readiness and application method support answer different questions. Existing operator health includes private process/backend detail. If a concrete application requires public health, define a sanitized, cached observation and shared gateway/local semantics as additive work.

## Optional faucet and local methods

Keep existing optional funding results with native names, fields and encodings. Include truthful evidence/error corrections; defer durable recovery and richer outcomes.

`faucet_requestAirdrop` takes `{ recipient, clientNonce }`; `dev_fundAccount` takes `{ recipient, rawAmount, clientNonce, faucet }`. `clientNonce` is 1..32 decoded bytes and must remain unchanged within the documented recovery scope. Local funding requires a positive canonical raw amount and the expected local faucet identity.

Result shapes:

```ts
type AirdropAttempt =
  | { attemptStatus: "pending"; recipient: Address; rawAmount: Amount;
      transactionHash?: Hash; retryAfterSeconds: U64 }
  | { attemptStatus: "succeeded"; recipient: Address; rawAmount: Amount;
      transactionHash: Hash; blockHeight: Height;
      execution: Extract<ReceiptExecution, { result: "return" }> };

type FundingSubmission = {
  recipient: Address;
  rawAmount: Amount;
  transactionHash: Hash;
  signedTransaction: Hex;
};
```

A pending hash identifies retained work, not admission. Succeeded requires a matching committed return receipt. Local funding returns submission/reconciliation evidence, including a committed failed transaction; inspect execution before asserting payment. Preserve retained signed bytes on success/error paths. Pending result delays remain seconds. Checked-convert producer error retryAfterSeconds to retryAfterMs, retaining service evidence; invalid or unrepresentable hints fail as backendDataInvalid. Never discard a policy minimum and retry earlier.

Map known airdrop_attempt_terminal/airdrop_transaction_failed, or an equivalent local stop, to -32030/fundingAttemptStopped. It means the service stopped advancing this attempt, not nonexecution or permission to replace. Preserve bounded serviceCode and available hash/receipt evidence. Read-only reconciliation can continue.

Parameter, overload and backend errors use the common catalogue. Policy refusals have no transaction admission field. Unknown service errors retain their bounded cause under backendUnavailable. Use -32031 for unresolved local funding with retained signed evidence. Outer wrappers must preserve a recognized stop and never convert it into retry permission.

```ts
type FundingAttemptStoppedData = {
  kind: "fundingAttemptStopped";
  serviceCode?: string;        // bounded diagnostic producer code, never parsed message
  transactionHash?: Hash;
  signedTransaction?: Hex;    // preserve existing local retained evidence
  receipt?: TransactionReceipt; // only an actually observed, matching receipt
};
```

No `admission` field or inferred stop-reason enum. Signed bytes require the matching hash. A receipt requires the matching hash and preserves its block/execution evidence. The current hosted error normally has no hash or receipt, so neither is required. Restrict this code to optional funding methods and include it in their generated schemas. An unresolved local wrapper may retain the stopped variant as a typed cause if prior dispatch remains unresolved; its decoder still stops automatic funding calls.

Hosted continuity is limited to the same store and retained record. Keep local journal recovery and the stopped-cause rule; stronger guarantees need a separate wire-version and durability decision. See [SDK funding recovery](#sdk-funding-recovery).

```ts
// dev_getPendingTransactionCount({})
type PendingCount = { count: U64 };
```

The local pending count is this runtime's pool observation. It is not a network-wide count or the next account nonce. The hosted gateway lacks its implementation and must not advertise it. Future hosted support needs a deliberately scoped method.

Remove the unsupported groundhog_dev_getTransactionDetail call/deploy probe and render receipts. Preserve `dev_detail::format_event` used by view; audit exported types/tests and thruput-trace references before deleting them. A future detail endpoint needs implementation and retention semantics. See the [probe/formatter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/cli/src/dev_detail.rs) and [local registration](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/node-supervisor/src/server.rs#L635).

## Error shapes and recovery

Use the standard JSON-RPC envelope with stable typed data.kind and operation-specific evidence. Messages are diagnostic. Application errors have generated typed data; dispatcher/framework errors may not. Cause, admission and recovery remain separate.

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
      "transactionHash": "<base58-transaction-hash>"
    }
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "error": {
    "code": -32012,
    "message": "Requested state height unavailable",
    "data": {
      "kind": "stateHeightUnavailable",
      "requestedStateBlockHeight": "814",
      "observedStateBlockHeight": "813"
    }
  }
}
```

| Condition | Recommended response |
| --- | --- |
| Wrong fields, type, bound, or cursor | `-32602`, typed `invalidParams` or `invalidCursor` data; field path when useful |
| Invalid signed transaction | `-32000`, `transactionInvalid`, existing typed bound diagnostics where available |
| Scheduler AuthenticationRejected | `-32001`, `transactionRejected`, `admission: "rejected"` |
| Local expiry/bound rejection before dispatch | Preserve the relevant transaction error code; `admission: "notAttempted"`. Local expiry currently also uses `-32001`. |
| Unavailable backend | `-32050`, `backendUnavailable`; on submit, preserve uncertainty if a write may have reached the backend |
| Backend deadline | `-32051`, `backendTimeout`; on submit use `admission: "unknown"` unless non-dispatch is proven |
| Overload before dispatch | `-32053`, `overloaded`, `admission: "notAttempted"` on submit when proven |
| Scheduler Retryable (capacity, not-ready or stale auth state) | `-32053`, `overloaded`, `admission: "rejected"`; coarse transient refusal, not proof of load |
| Scheduler InternalError | `-32050`, `backendUnavailable`, `admission: "rejected"`; protocol supplies definite nonadmission for this attempt |
| State freshness floor unmet | Proposed `-32012`, `stateHeightUnavailable`; move off colliding `-32011` |
| No receipt at a successful lookup | `result: null` |
| Unknown retained transaction | Status result `unknown`, only after successful misses |
| Simulated abort/fault or committed revert/fault | Ordinary typed execution result |
| Unsupported optional method | Standard `-32601` method-not-found; do not advertise it |
| Malformed backend data or proven unavailable history | Proposed `-32054` / `backendDataInvalid` and `-32013` / `historyUnavailable`; never silent success |

### Admission evidence

Classify the current submission attempt from evidence at the failure site, independently of its numeric code:

| Admission value | Exact meaning |
| --- | --- |
| `notAttempted` | This attempt sent no submission to the admission backend. Local invalid XDR, invalid bounds, expiry rejection and proven pre-dispatch overload belong here. |
| `rejected` | The admission backend explicitly refused this attempt. AuthenticationRejected, Retryable and InternalError replies belong here; this does not imply permanent invalidity. |
| `unknown` | No definitive admission outcome is available. Use conservatively for possible-dispatch timeout, connection loss, dropped replies and malformed outcome batches. |

These are attempt-level statements. None proves that an earlier submission with the same hash did not commit. The body hash excludes authentication bytes; one envelope's rejection cannot disprove another envelope's acceptance.

Codes do not establish admission: backend timeout/unavailable arise before and after dispatch. Overloaded also covers scheduler not-ready/stale-auth refusal. Missing root-account capacity and dependency failures are conflated today, so do not invent insufficientFunds. Emit finer context only from evidence.

Scheduler mapping: Admitted→success; AuthenticationRejected→-32001/rejected; Retryable→-32053/rejected; InternalError→-32050/rejected. Wrong cardinality, unknown values and transport loss after possible dispatch remain unknown. Local expiry uses -32001/notAttempted. See [scheduler outcomes](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/sched_gateway.x#L20-L39).

Preserve phase/evidence across gateway/local queues and outer deadlines. Test queue expiry, cancellation before dequeue, precheck timeout, sink handoff, lost replies and wrong cardinality. notAttempted requires proof no worker can later dispatch; sink entry alone proves neither outcome. After possible dispatch, use unknown unless stronger evidence arrives. Attach a hash only after valid computation; earlier timeouts and dispatcher/transport errors can lack it. Clients retain their own evidence. See [submitter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-gateway/src/submitter.rs).

### Typed details

Recommend one stable kind per error family, with structured refinements. For example:

```json
{
  "code": -32602,
  "message": "Gas limit is outside the allowed range",
  "data": {
    "kind": "invalidParams",
    "path": "/params/invocations/0/gasLimit",
    "reason": "outOfRange"
  }
}
```

Structural serde failures may return only `{ kind: "invalidParams" }`. Emit optional reason/path only from reliable typed validation or captured path evidence. Do not parse serde messages or build a second general key/schema validator solely for richer errors. Discovery refresh does not depend on unknownField. The example above assumes an explicit typed gas-bound check.

`path` is a JSON Pointer rooted at the full RPC request; `reason` is a stable enum, not message text. Keep existing transaction-bound diagnostics under a typed `detail` object rather than flattening away their reason and values. Preserve scheduler source/code when actually supplied; do not parse a backend message to invent a cause. Include freshness requested/observed heights and retained local-funding `{transactionHash, signedTransaction}` evidence in their respective error variants.

`invalidCursor` refines the InvalidParams code with a proven reason such as malformed, unsupported version or query mismatch. Do not guess expiry/reset when only generic invalidity is known. The proposed allocations and optional reason enums follow below. A historical-data error must not become an empty successful page.

An optional `retryAfterMs` is a canonical decimal u64 delay hint supplied only by real policy/evidence. It is neither an acceptance guarantee nor an expiry extension. The SDK honors the minimum delay subject to caller cancellation/deadline; if there is insufficient time, it stops. Without a hint, use capped exponential backoff with jitter. Current scheduler refusals do not provide a duration, so do not invent one.

### Proposed common catalogue and optional reason enums

These are native error allocations, not EIP-1474 compatibility. Optional funding uses the same catalogue with the narrow evidence rules above.

| Code | `data.kind` | Selected role |
| --- | --- | --- |
| `-32602` | `invalidParams` or `invalidCursor` | Invalid named request or unusable continuation token. Two tagged refinements share the standard parameter-error code. |
| `-32000` | `transactionInvalid` | Invalid signed transaction framing, envelope structure or executor-bound checks. |
| `-32001` | `transactionRejected` | Proven local expiry or scheduler AuthenticationRejected; admission evidence distinguishes them. Retryable/InternalError use their own families. |
| `-32031` | `localFundingUnresolved` | Retained signed funding evidence without a resolved outcome. Preserve `transactionHash` and `signedTransaction`. |
| `-32030` | `fundingAttemptStopped` | Selected optional funding-service stop; no transaction admission/nonexecution claim. Preserve available service cause and evidence. |
| `-32012` | `stateHeightUnavailable` | Selected snapshot is below `minStateBlockHeight`. Preserve requested/observed state heights. Replaces the application use of `-32011`. |
| `-32013` | `historyUnavailable` | A valid historical query/continuation cannot be served because required history is unavailable, when proven by the backend. |
| `-32050` | `backendUnavailable` | Required backend/dependency unavailable. |
| `-32051` | `backendTimeout` | Required operation exceeded its deadline. |
| `-32053` | `overloaded` | Temporary gateway/backend refusal, including scheduler capacity, not-ready or stale-auth state. No finer reason inferred; admission evidence remains independent. |
| `-32054` | `backendDataInvalid` | Backend response cannot satisfy the expected data contract. This includes unsupported backend values, not only corruption. |

Standard `-32700`, `-32600`, `-32601` and `-32603` retain parse-error, invalid-request, method-not-found and internal-error meanings. These and framework/transport failures need not carry our typed data. Do not use internal-error as a substitute for an already identified backend-data failure.

Retire `-32003` (old local funding unresolved) without reuse. Retire the application meanings of `-32002` (transaction not found), `-32010` (block not found) and `-32052` (registered stub). Do not reassign them to new application errors. Successful receipt absence remains null, status misses remain `unknown`, and unsupported methods are unregistered and return `-32601`. Required advertised methods with a temporary failed backend return a backend error. Deferred block methods need a separate contract before future delivery.

#### Why freshness needs a new code

Pinned jsonrpsee 0.25.1 uses -32010 for oversized batch requests and -32011 for oversized batch responses. The latter can follow member dispatch, so freshness moves to -32012 and unavailable history to -32013 in the coordinated major.

Keep framework size errors distinct: `-32007` oversized request, `-32008` oversized response, `-32010` oversized batch request and `-32011` oversized batch response. A response-size failure can occur after writes. A whole-batch error may have `id: null` and replace all member results; retain uncertainty for each potentially dispatched write. The SDK must not blindly replay the entire batch. These paths may supply string data rather than our typed object.

The framework also defines generic `-32000`/`-32001` constants, but this review found no automatic production server/core emission in the pinned dependency; current handlers return explicit application errors. Keep the transaction allocations, audit generic conversions, and add an upgrade check for emitted middleware codes. If a future emission conflicts, normalize it at the server boundary or make an explicit wire-version change. Do not rely on messages to disambiguate.

Changing the freshness code belongs in the coordinated major release. An SDK handling old wire versions must not infer freshness from `-32011` alone. Update per-method OpenRPC mappings, gateway outcome labels, dashboards and all SDK/CLI error decoders together.

Sources: [current error catalogue](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/error.rs), [shared batch overflow](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-server/src/batch.rs#L158), [dependency lock](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/Cargo.lock#L2073). The dependency constants and emission helpers were inspected in the locally installed 0.25.1 source.

#### Minimal reason vocabulary

Make `reason` optional and emit it only from typed evidence. Omission means no finer classification is available; do not add a generic `unknown` reason to every enum. A present reason is stable camelCase. `kind` identifies the family, and `reason` refines it. Neither field alone authorizes transaction replacement.

| Kind | Proposed optional `reason` values | Meaning / evidence boundary |
| --- | --- | --- |
| `invalidParams` | `missingField`, `unknownField`, `invalidType`, `invalidFormat`, `outOfRange`, `conflictingFields`, `invalidValue` | Shape, encoding, bound or semantic validation. Use `invalidValue` for a known unsupported enum/value or invalid value combination not captured by another reason. `path` is optional when the error spans fields or no precise path is available. |
| `invalidCursor` | `malformed`, `unsupportedVersion`, `queryMismatch` | The cursor owner supplies the distinction. Omit reason for undifferentiated invalidity. Do not invent expired/reset reasons or decode backend cursor internals in the gateway. |
| `transactionInvalid` | `invalidEncoding`, `malformedTransaction`, `invalidAuthEnvelope`, `transactionTooLarge`, `executorBoundViolation` | Invalid signed-transaction hex; undecodable/noncanonical XDR or trailing bytes; claim/key structural mismatch; envelope size rejection; or a typed executor-bound check. Signature verification remains the backend's job. |
| `transactionRejected` | `expired`, `authenticationRejected` | Direct local expiry comparison or the backend's coarse authentication refusal. Neither establishes an earlier attempt's outcome. |
| `historyUnavailable` | `outsideRetention` | Only when the backend proves required history precedes the retained range. Other proven unavailability can omit the reason until a useful finer contract exists. |
| `backendDataInvalid` | `invalidEncoding`, `invalidCardinality`, `unsupportedValue`, `inconsistentResult` | Undecodable data; wrong result count; unsupported tag/version/value; or conflicting data that violates an enforced invariant. Do not label a backend error return as malformed solely because it is an error. |

`backendDataInvalid` with `unsupportedValue` applies only when the value cannot be represented under the method's proposed result contract. Preserve deliberate `other`/`unknown` projections, such as receipt execution outcomes; a new backend enum value is not automatically an RPC error.

`localFundingUnresolved`, `stateHeightUnavailable`, `backendUnavailable`, `backendTimeout` and `overloaded` do not need a reason enum at this stage. Their typed evidence, heights or optional policy-backed `retryAfterMs` already describe the useful distinction. Admission must not be encoded again as a timeout/overload reason.

For `executorBoundViolation`, retain `detail.reason` and its existing variant fields:

| `detail.reason` | Existing fields to preserve |
| --- | --- |
| `tooManyContracts` | `attached`, `max` |
| `contractTooLarge` | `index`, `sizeBytes`, `maxBytes` |
| `duplicateContract` | `index`, `firstIndex` |
| `rv32MemoryTooLarge` | `index`, `memoryBytes`, `maxBytes` |
| `rv32CodeTooLarge` | `index`, `codeBytes`, `maxBytes` |

Those existing bound quantities are u32 JSON numbers; new 64-bit-or-wider quantities remain canonical decimal strings. No gas-price or fee detail is introduced. Preserve raw typed scheduler source/code where supplied. Do not translate coarse authentication refusal into bad-signature, wrong-key, unsupported-scheme or wrong-network claims. The existing replay-protection conflict combines expiry, prior consumption and duplicate consumption within one transaction, so it cannot justify `alreadyExecuted` or a more specific expiry reason.

```json
{
  "code": -32602,
  "message": "Cursor does not match the query",
  "data": {
    "kind": "invalidCursor",
    "reason": "queryMismatch",
    "path": "/params/cursor"
  }
}
```

```json
{
  "code": -32013,
  "message": "Requested history is outside retention",
  "data": {
    "kind": "historyUnavailable",
    "reason": "outsideRetention",
    "oldestIndexedBlockHeight": "900",
    "latestIndexedBlockHeight": "1200"
  }
}
```

History bounds are optional and must come from authoritative observations for that query. A valid cursor can refer to history that has been pruned; that is unavailable history, not malformed input. An indexer outage remains backendUnavailable. An explicit anchor above the current serving tip still follows the proposed boundary rules: ascending exhausts, descending starts at the tip. Do not introduce a rangeNotIndexed error that changes those rules. None of these errors permits silent restart, gap skipping or checkpoint advancement.

#### Compatibility and implementation gates

Error decoders should tolerate additional fields and unknown reason values, preserving the raw error for diagnostics. A new reason must not make a known family undecodable. Unrecognized kind/admission, malformed typed data, or a code/kind mismatch cannot authorize automatic submission; preserve uncertainty and surface the error. An unknown refinement within a known transient read family can retain bounded read retry. Missing/non-object framework data is a supported input, not a decoder crash. The selected result-enum policy above applies separately to successful RPC results.

Generate all 12 application data kinds under 11 named code components from Rust,
using per-code unions such as invalidParams|invalidCursor and valid
operation-specific evidence. Each method lists unique numeric error codes.
Generate TS schemas/fixtures from owner types; test examples, unknown refinements,
missing framework data and collisions. Match gateway metrics to the catalogue.
[OpenRPC](https://spec.open-rpc.org/) defines the method/error structure;
`x-data-schema` is our schema extension.

### SDK recovery policy

A generic `retryable` boolean cannot distinguish read retry, same-byte submission retry and transaction replacement. Prefer the following bounded policy:

| Observation | SDK action |
| --- | --- |
| Invalid parameters, transaction structure or authentication | Stop automatic retries of the same invalid request. Surface typed detail; preserve any earlier uncertainty. |
| Read timeout, transient unavailable backend or overload | Retry the read with backoff within caller budget. A newer read can observe newer state; it is not an exact-snapshot replay. |
| Freshness floor unmet | Retry later or use an appropriately verified endpoint within budget. Do not silently lower the requested floor. |
| Proven transient submission refusal | Before recovery qualification, repeat only a proven never-dispatched attempt with no prior uncertainty. After qualification, exact-byte retry must satisfy all preconditions below. |
| Submit timeout, connection loss or ambiguous response | Retain uncertainty and reconcile original hash. No automatic redispatch until runtime recovery is qualified, then only under all preconditions below. |
| `pending` | Poll within the caller's deadline; no replacement. |
| `committed` | Preserve commitment within the same chain instance; obtain receipt separately. Never resubmit to resolve a missing receipt. |
| `retry` | Preserve source/code; read/reconcile. Exact-byte resubmission needs runtime qualification and all preconditions below. |
| `dropped` | Stop automatic submission. Surface the reason. Reconcile earlier uncertainty before an explicitly approved replacement. |
| `recentFailure` | Advisory evidence, not final failure or an instruction to rebuild. Continue reconciliation. |
| `unknown` or unfamiliar status/error | Preserve evidence and uncertainty. Known transient conditions can be retried under policy; unfamiliar outcomes do not authorize automatic resubmission or replacement. |
| Receipt with failed execution | Return the committed receipt. A separate success assertion may throw; do not repeat the business action automatically. |
| Invalid cursor, unavailable history or malformed backend data | Do not advance checkpoint or silently restart/skip. Require correction or explicit recovery. |

Runtime duplicate correctness is mandatory even with SDK retries disabled. Automatic exact-byte redispatch additionally requires nomination-between-chunks, full-queue, abandonment/reenqueue and postcommit replay qualification. Until then, automatic repeats are limited to proven never-dispatched attempts with no earlier uncertainty. A later rejection never clears earlier uncertainty.

After qualification, exact-byte recovery requires identical approved bytes/hash,
verified expected network, unchanged chain instance, unexpired validity, no known
commitment, bounded attempts/backoff and continuing caller/provider permission.
Passing expiry prevents future inclusion but cannot disprove prior commitment.
The [replay key](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/transaction.x#L108)
includes root address, nonce and expiry; changing expiry can create a second valid
transaction. Re-signing or changing nonce/expiry requires reconciliation and fresh
approval where applicable. Replacement helpers remain deferred. Handle chain
reset explicitly; a missing record outside retention cannot prove nonexecution.

```ts
type SubmissionEvidence = {
  signedTransaction: Hex;
  transactionHash: Hash;
  networkHash: Hash;
};
type KnownCommitment = {
  networkHash: Hash;
  transactionHash: Hash;
  blockHeight: Height;
};
```

Retain immutable SubmissionEvidence before dispatch with a local onSigned persistence hook. Submission/observation errors, including cancellation after signing, carry it with strongest commitment and original cause. Explicit same-byte submission still obeys identity/replay preconditions; never reuse consumed wallet approval or bypass session revocation. Optional locally known gasLimit is diagnostic, not proof of out-of-gas.

Cancellation stops waiting, not submission. A lost batch response leaves each dispatched write uncertain. Correlate members by JSON-RPC ID, not response order, and apply recovery policy separately.

### SDK funding recovery

Keep requestAirdropAttempt as a single call. High-level requestAirdrop also calls
faucet_requestAirdrop once on hosted and local endpoints:

- Succeeded completes after validation.
- Pending with a hash starts read-only status/receipt waiting within the caller
  budget, followed by the execution-success assertion.
- An RPC error or pending without a hash stops automatic funding progression.
  Preserve cause, recipient, nonce, expected network, last response and any
  hash/commitment, including on timeout, cancellation and terminal failure.

Persist the nonce before dispatch. Never automatically repeat the faucet call,
including policy refusals or pending polls. HTTP 429, serviceCode and retry hints
prove neither nonexecution nor durable continuity; they guide explicit retries.
No funding-dispatch field or automatic continuity option is added this major.

A helper can construct AirdropSucceeded from the pending recipient/amount/hash
and a matching committed return receipt. Failed/unsupported receipts throw typed
outcomes carrying the request and receipt. This observes success; it does not
prove the service persisted a terminal record. Missing evidence stays unresolved.

Explicit resume uses the original recipient/network/nonce and retained evidence.
Calling the hosted service again requires caller opt-in to duplicate-test-funding
risk when continuity is unproven. The current in-memory store cannot establish
continuity across restart, independent replicas or terminal retention expiry.
An unchanged URL or later store miss does not clear uncertainty.

Explicit dev_fundAccount retries remain scoped to the persistent journal and
reuse the complete request, including faucet identity and amount. Preserve signed
evidence and reconcile the original hash. This does not change the SDK single-call
faucet composition or authorize a new nonce after uncertainty.

Sources: [hosted producer](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/airdrop.rs),
[retry hints](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/error.rs),
[store](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/lib.rs#L91),
[retention](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/policy.rs#L389),
[local pending path](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/node-supervisor/src/airdrop.rs#L528-L590),
and [local journal](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/node-supervisor/src/funding_journal.rs#L30).

## Source baseline

Address naming was rechecked against origin on 15 October 2026: protocol
`47f88b2`, hogd `15d53691` and tools `d0a8cdc9`. The broader review below retains
its original baseline.

Source review dated 15 October 2026: tools `d0a8cdc9`, hogd `15d53691`, protocol
`af043b5c`, TS SDK `8ce62f45`, examples `c8ba8730`, chuckd `6db13472`, packaging
`73566ec9` and ops `f03432c9`. Links above pin the relevant source. This was source
inspection, not runtime qualification. The baseline does not describe subsequent
implementation progress; current work and validation are tracked in Linear.

Dependency mechanics were checked in installed jsonrpsee 0.25.1, serde 1.0.229
and serde_json 1.0.151 sources. The OpenRPC reference is current 1.4.x;
implementation must validate against its selected meta-schema, including any
retained 1.2.6 version.
