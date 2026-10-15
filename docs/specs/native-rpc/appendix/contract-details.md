# Appendix: proposed RPC contract details

This is the detailed target behind the [API proposal](../api-proposal.md).
The [delivery plan](../delivery-plan.md) owns scope and dependencies;
[evidence](review-evidence.md) records the source checks and finding dispositions.
These are proposed semantics for team review, not shipped behaviour or team
approval. Implementation and Linear changes remain on hold.

The type sketches and placeholder examples describe the target contract. They
are not a second implementation schema. Rust definitions will generate the
OpenRPC and JSON Schema artifacts when implementation is authorized.

## Proposal decisions

The proposal keeps separate status/receipt methods, explicit height roles,
nested execution with sibling events/gas, metering-only gas, start/cursor
pagination and independent cause/admission/recovery evidence. The source review
refined the following details; finding IDs link back to the evidence appendix.

| Decision | Selection |
| --- | --- |
| TT-1 strict params / ETH-5 | Parameterized calls require strict named objects. Noarg canonical {}, with omission/[] accepted and null normalized as omission by the transport. No unknown fields or nonempty noarg arrays. |
| STL-1 identifier boundary | Canonical case-sensitive base58 RPC/provider identifiers; explicit typed human-input helpers convert C/G. S belongs only to secret import/export. Opaque bytes remain hex. |
| TT-3 registration | Exact configured-plane map under discovery; full announced profile requires all 15 core methods. Partial developer endpoints remain valid. |
| ETH-2 / TT-6 / DOC-1 | Receipt current labels return/abort/fault match simulation, with authoritative execution/root-output consistency checks. No fabricated fault kind or revert message. |
| ETH-3 | localFundingUnresolved moves to -32031; retire -32003. Keep -32030 service stop. |
| ETH-9 / SOL-6 / CR-9 | accountNonce in simulation; remove unproduced invalid status; KnownCommitment includes networkHash. |
| ADM-1/2 / STAT-1 | Explicit scheduler outcome mapping, no hiding indexer failure with negative status, automatic exact-byte recovery only after runtime qualification and replay preconditions. |
| HIST-1/2/3 / SOL-2/3 / PROTO-1 | Expanded fail-closed history minimum and regressed-W rejection. Typed protocol errors/query binding required final. Candidate cap/direct seek in slice 2; general time/byte budgets, cancellation and durable reset recovery follow. |
| ETH-8 / CR-8 / STL-6 | Keep existing assertTransactionSuccess, waitForExecution and prepareTransaction helpers with precise native behavior. |
| EVT-1 | Freeze token event/error signature identity at 0.1 and groundhog-erc20-if; run existing native/RV32 parity coverage. |
| Follow-up receipt review | Require return/abort bytes; validate backend log identity/height; error observation contains only hash/height; preserve commitment in Rust and TS. |
| Follow-up funding review | One SDK faucet_requestAirdrop call on hosted/local endpoints, then read-only waiting when pending has a hash; no automatic service repeats this major, including refusal/pending. Retain explicit resume, evidence and hints; add no funding-dispatch field. |
| Follow-up transport review | Thin flat-DTO Rust clients, optional evidence-backed parameter details, generated per-code error unions, raw-preserving response readers and bounded metrics. |
| Follow-up consumer review | Include chuckd/Go/final pins and all three proxies; remove unserved CLI detail probe. Exact reviewable PQ/encoding diffs required, branch publication optional. |
| DEL-3/6 | Manual publication freeze while buildable slices land; controlled artifact/bootstrap order; one final set for fresh-devnet qualification. |

The delivery plan owns assignment and release gates. Exact pins, shared-file owners, coordinated major, qualification owner/workload/thresholds remain outstanding facts. The current-source findings do not establish runtime readiness.

## Method map

Remove `groundhog_` from all core methods. Keep explicit `faucet_` and `dev_` groups for optional services. All request columns below describe `params`, and all response columns describe `result` inside the JSON-RPC envelope.

The full-service application endpoint should support the 15 core methods below. A deliberately partial endpoint advertises only configured support; temporary backend failure does not remove methods from discovery. Local and hosted implementations must implement the same contract for methods they share.

| Current suffix | Selected name | Parameters | Result | Disposition |
| --- | --- | --- | --- | --- |
| `version` | `getVersionInfo` | `{}` | `VersionInfo` | Core |
| `networkInfo` | `getNetworkInfo` | `{}` | `{ networkHash }` | Core |
| `chainTip` | `getChainTip` | `{}` | `{ blockHeight, blockHash }` | Core |
| `chainConfig` | `getChainConfig` | `{}` | `ChainConfig` | Core |
| `serverLimits` | `getServerLimits` | `{}` | `ServerLimits` | Core |
| `sendRawTransaction` | `sendTransaction` | `{ signedTransaction }` | `{ transactionHash, admission: "accepted" }` | Core |
| `getTransactionStatus` | `getTransactionStatus` | `{ transactionHash }` | `TransactionStatus` | Core |
| `getTransactionReceipt` | `getTransactionReceipt` | `{ transactionHash }` | `TransactionReceipt` or `null` | Core |
| `view` | `simulateInvocation` | `Invocation` plus optional `minStateBlockHeight` | `{ stateBlockHeight, output, events, gasUsed }` | Core on full-service endpoint |
| `viewBatch` | `simulateInvocations` | `{ invocations, minStateBlockHeight? }` | `{ stateBlockHeight, results }` | Core on full-service endpoint |
| `getStorageAt` | `getStorage` | `{ address, key }` | `{ stateBlockHeight, item }` | Core |
| `getBalance` | `getBalance` | `{ address }` | `{ address, stateBlockHeight, rawBalance }` | Core |
| `getAddressAuthData` | `getRegisteredAuth` | `{ address }` | `{ address, stateBlockHeight, configuration }` | Core |
| `getEvents` | `getEvents` | `EventQuery` | `EventPage` | Core; correctness gates below |
| `getTransactionsByAddress` | `getTransactionsByRootContract` | `RootContractQuery` | `TransactionRefPage` | Core; correctness gates below |
| `requestAirdrop` | `faucet_requestAirdrop` | `{ recipient, clientNonce }` | `AirdropAttempt` | Optional hosted/local faucet |
| `fundAccount` | `dev_fundAccount` | `{ recipient, rawAmount, clientNonce, faucet }` | `FundingSubmission` | Local only |
| `pendingTransactionCount` | `dev_getPendingTransactionCount` | `{}` | `{ count }` | Local implementation only; omit hosted stub |
| `getBlockByNumber` | No method in this release | None | None | Defer real `getBlock` until backend exists |

The code also contains a legacy `groundhog_dev_getTransactionDetail` consumer and result types, but no server registration or handler was found in the reviewed tools/hogd source. Remove the unsupported call/deploy probe and its diagnostics; render the receipt instead. Preserve the event formatter used by `view`. Delete public detail DTOs and dependencies only after a reference/public-API audit. A future detail capability needs its own implementation and retention contract. Keep `firehose_*` outside this contract until the separate feed decision is made.

Do not add `getTransaction`, `prepareTransaction`, `estimateFee`, public `getHealth`, an account nonce method, historical state selectors, or confirmation levels in this release. Each would need a distinct useful contract, not just a familiar name.

### Naming tradeoffs

- `sendTransaction` with `signedTransaction` is familiar and explicit about who signs. Retaining `sendRawTransaction` is defensible but adds little once the parameter states the boundary.
- `getChainTip` describes the supplied height/hash. `getLatestBlock` sounds like a block body or richer header, which this method does not provide.
- `simulateInvocation` accurately covers discarded writes and emitted events. `readContract` understates the behavior; `simulateTransaction` promises checks this operation does not perform.
- `getTransactionsByRootContract` is long but accurate. `getContractTransactions` is shorter, yet can still imply nested calls. Neither is a wallet activity API. A wallet contract can be the root target too.
- `getRegisteredAuth` states what is read. It does not promise account existence, recovery of a pubkey account's key, or general wallet policy introspection.
- Keep `getBalance` for native HOG only. Token balances use contract calls. Its documentation and SDK types state the unit explicitly.
- Select `dev_` for local funding and pool observation. Execution inspection can join only after implementation; the namespace alone is not a capability promise.

## Shared JSON contract

**Proposed request rules:** use JSON-RPC 2.0 with one named object for parameters. No `{request: ...}` or `{query: ...}` wrappers. Nested objects below carry real structure, such as an invocation's assumed authorization or pagination start.

- Canonical no-argument call: `params: {}`. Accept omitted `params`, `[]` and noarg `null` normalized to omission. Reject nonempty objects/arrays. For parameterized methods require a named object; omitted/null/array params are invalid. This narrow noarg exception is deliberate: jsonrpsee 0.25.1 collapses absent and null before dispatch. Do not fork the transport or add a second envelope parser for this distinction.
- Optional request fields are omitted. Reject explicit null unless a field specifically allows it. Reject unknown request properties at every boundary, including tagged-union variants.
- Outputs tolerate additional fields. Readers preserve or surface unknown union variants as unsupported data; they must not infer success, absence or permission to replace a transaction.
- Use base58 for typed addresses, keys, hashes and signatures. Preserve hex for signed XDR, calldata, return/abort buffers, raw storage keys, topics, event data and opaque cursors. RPC/provider typed identifiers accept canonical base58 only, not C/G wire aliases. Explicit typed human-input SDK/CLI/wallet helpers may convert C addresses or G keys before serialization; S is only explicit secret import/export. No legacy hex fallback for identifiers. Coordinate this clarification with GH-502/GH-503/GH-507.
- All 64-bit-or-wider integers are canonical decimal strings, including small values. Smaller bounded integers are JSON numbers. Domain bounds remain enforced; a string is not permission to accept arbitrary precision everywhere.
- `transactionHash` always means the canonical hash of the unsigned transaction body. It is neither a signature nor an identity for the exact signed envelope.
- `blockHeight` identifies a committed block; `stateBlockHeight` identifies the committed snapshot read. `attemptBlockHeight` identifies a failed proposal attempt. Separate reads can return different heights.
- A successful JSON-RPC transport response does not mean successful transaction execution. Execution outcomes belong in results; inability to answer belongs in errors.

The type sketches below are design notation, not a second hand-maintained schema. Rust traits/types will generate the OpenRPC specification and JSON Schema. JSON examples use angle-bracket placeholders and illustrative bytes, not executable transaction fixtures.

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

The parameterized-method migration is real transport/proxy work. Strict request DTOs own omission/null/unknown-field validation. Use a shared raw-Params registration wrapper, typed ToRpcParams client and generated by-name descriptors plus whole-object x-params-schema from those same DTOs. Preserve backend traits; avoid duplicate schemas. The pinned `#[rpc(client)]` macro wraps each argument even when its DTO implements `ToRpcParams`. Replace that client path with thin `ClientT::request(method, requestDto)` adapters. The slice-1 spike chooses small generated or explicit adapters, not a mandatory macro-engine rewrite. Add standard `paramStructure: "by-name"` for parameterized methods alongside `x-params-schema`; document the separate noarg normalizations. Validate against the selected OpenRPC meta-schema. A feasibility spike must demonstrate gateway/local/client/OpenRPC parity before broad migration. The noarg exception does not permit positional arguments on other methods.

Use distinct SDK semantic types/constructors for keys, public-key hashes and derived addresses. Never implicitly turn a G/public key into an account address. Valid32-byte text cannot always reveal a mistaken semantic type. The body hash excludes network identity; signatures bind it. Records use verified networkHash plus transactionHash and explicit reset invalidation, even if networkHash is reused.

### Height fields

Use `blockHeight` for block identity and committed inclusion, `stateBlockHeight` for the snapshot behind a state read or simulation, and `attemptBlockHeight` for an advisory failed proposal attempt. Use `minStateBlockHeight` for the simulation freshness floor. Page bounds keep the explicit `oldestIndexedBlockHeight` and `latestIndexedBlockHeight` names; they are serving bounds, not processed checkpoints. Inside `execution.observedTip`, use the same `{ blockHeight, blockHash }` shape as the chain tip.

A uniform `height` would be shorter, but would leave its meaning to the enclosing method or object. The proposed explicit fields preserve the distinction when values reach SDK objects, logs or application state. Do not add a generic `context` wrapper or redundant derived heights. For example, a receipt at block 812 and balance read at state block 813 are compatible observations, not a promise that the balance includes only that receipt. Simulation reads state H and runs in execution context H+1; report the actual state height H.

All public heights remain canonical decimal strings bounded to nonnegative signed-64 values. No field name creates an exact-height selector, cross-call snapshot guarantee or block-hash provenance the backend does not supply.

## Submit and observe a transaction

### Submission

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
    "transactionHash": "<base58-transaction-hash>",
    "admission": "accepted"
  }
}
```

Return success only after authoritative admission. Accepted includes an already-pending duplicate; it does not assert a newly queued transaction, continued mempool presence, commitment, or successful execution. Do not add an `alreadyCommitted` outcome without evidence from the backend.

The object adds a field compared with today's bare hash, but makes the success guarantee visible. `admission` avoids overloading the lifecycle field `status`.

The SDK computes the body hash and retains the exact approved signed bytes before transmission. On an uncertain submission, reconcile that hash. Automatic exact-byte redispatch is disabled until the runtime recovery qualification below passes; later recovery must meet every listed precondition. A server may return no JSON at all after a disconnection, so this rule cannot depend on receiving structured error data.

Do not automatically refresh expiry or nonce. The native replay key includes the root invoked address, nonce and expiry height. Changing expiry with the same nonce can create a second valid transaction. Rebuilding after an uncertain outcome needs application-level reconciliation and a new approval where applicable. Expiry passing prevents future inclusion of the old transaction; it does not prove that it never committed. [Replay protection source](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/transaction.x#L108).

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

Indexed commitment wins. If the indexer fails, scheduler commitment may still answer; pending may report provisional presence without proving no earlier commitment. If the indexer fails and the scheduler returns retry/dropped/recent diagnostic/miss, propagate the read failure. Never hide it as a negative lifecycle result. `unknown` requires successful retained-source misses. Advisory recentFailure is not final failure. Preserve raw scheduler diagnostics, and treat other/unfamiliar tags as unsupported.

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

**Proposed:** preserve the proposed nested `execution`, with current `return`/`abort`/`fault` labels shared with invocation output. Return/abort bytes are required for the respective receipt and simulation variants; `"0x"` is empty. Fresh devnet needs no historical missing-buffer branch. Missing bytes fail backend/response validation. Keep events/gasUsed at receipt level and omit redundant success/lifecycle flags.

The transaction ExecResult and root frame are independent inputs. Validate their supported combinations before attaching a buffer; root return alone never establishes successful transaction execution. A commit-stage failure can follow root Return, so do not require every failed ExecResult to have Fault root output. Contradictions fail with backendDataInvalid/inconsistentResult. The current deterministic fault kind is manufactured as `fault` by the adapter, not a backend diagnostic; remove it. The existing revert message is never produced; remove it. Do not infer out-of-gas.

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

Before parsing events or checking ExecResult/root-output consistency, the shared hogd adapter must validate `log.tx_hash == tx_hash_to_wire(tx_hash)` and a nonnegative height within the wire Height range. If either check fails, return backendDataInvalid/inconsistentResult without observation. Only after both checks pass may a later event-parse or execution-consistency failure attach `{ transactionHash, blockHeight }`. It currently echoes the request hash, making a client-only equality check insufficient. A mismatch is backendDataInvalid/inconsistentResult with no commitment observation for that requested hash. Error observations contain only validated hash/height, never partial execution or gas. The SDK checks observation hash/network context and preserves independently known commitment; receipt decoding failure cannot authorize resubmission. Single/batch simulation fails the whole result on invalid events; no partial successful result.

`gasUsed` is the measured execution work, expressed in gas units. `gasLimit` is an execution budget. Neither field is denominated in HOG, represents a payment, or reports tokens burned. Do not derive a fee or debit by multiplying gas units by an invented gas price. The root gas budget still reserves scheduling capacity against free HOG; no burn does not mean no balance prerequisite. Funding only a called contract does not supply the root's capacity. Keep storage collateral and its native-token amounts separate. Preserve this distinction in the SDK onboarding example, without adding a fee-estimation RPC.

`null` means this successful lookup did not produce a committed receipt. It does not say pending, never submitted or definitively uncommitted. A `committed` status followed by a null receipt is valid. An indexer outage must not become null. A receipt includes execution results and events, not the transaction body, detached key or signatures.

The last historical outcomes need careful reader support. The protocol retires `failedGasSponsorship` and excludes `nonDeterministicError` from valid proposals. Preserve existing decoding without presenting them as ordinary current committed results. Investigate unexpected emission; do not invent gas sponsorship workflows from the enum. [Execution protocol](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/exec_result.x).

### Why retain two reads

A combined getTransaction would offer one read path, but needs explicit commitment, receipt-availability, backend-failure and payload-cost rules. It can also imply body retrieval that the current backend cannot supply. Separate status and receipt reads keep polling small; SDK composition handles the ordinary application flow.

**Proposed SDK contract:** `waitForCommitment` returns `KnownCommitment = { networkHash, transactionHash, blockHeight }`; existing `waitForTransactionReceipt` returns every committed receipt, including failed execution. Keep existing `assertTransactionSuccess` and `waitForExecution`; the assertion throws with the full failed receipt, and waitForExecution composes receipt waiting and assertion; the known `other` variant and unfamiliar execution tags produce a typed unsupported outcome with raw data. Wait helpers only observe. They never sign or submit.

Retain the existing 30-second default timeout and configurable 250 ms polling interval, with bounded jittered backoff for transient read failures. The caller can set its deadline and cancel. A timeout/error preserves the original hash, last observation and any known commitment; it distinguishes receipt unavailability after commitment. Never resubmit merely because a receipt has not appeared. Existing wallet call/deploy flows compose these primitives. Do not require a new send-and-wait API or SDK-wide workflow rewrite for GA. Fix the current suggestion that `dropped` requires modifying a transaction. Exact-byte recovery remains explicit and owned by the signing/submission flow.

`waitForTransactionReceipt` accepts optional `knownCommitment: { networkHash, transactionHash, blockHeight }` from `waitForCommitment` in the same verified network context. Validate its hash against the requested hash and its networkHash against the verified expected network. This is not a chain-instance identifier; invalidate reset-generation records explicitly. Once supplied or observed, commitment cannot be downgraded by later null/unknown/dropped status; interruption becomes receipt-unavailable with retained commitment. Require committed status to contain its height. No global transaction cache is required. Apply the same strongest-observation rule to the shared Rust `poll_receipt` path used by CLI, hosted faucet, simulator/testkit and embeddable clients. Cover Committed→Unknown, Committed→Retry/Dropped, Committed→lookup failure and RecentFailure→Committed→timeout. Once commitment is supplied or observed, later Retry/Dropped must not take the early `ReceiptPollError::Status` exit: continue waiting for the receipt within the caller budget. Unknown and status lookup errors must not overwrite the committed observation. Earlier RecentFailure cannot replace commitment at timeout. Preserve commitment in every error carrier, including a receipt lookup failure. Receipt-first versus status-first polling is a performance choice; after commitment, weaker status must not change recovery evidence.

Compare the submission response hash with the locally computed body hash. Preserve the receipt decoder's existing request/hash equality check. A well-formed but different hash is an invalid response, not a successful submission/receipt observation; retain the original identity and signed bytes without granting automatic resubmission permission. A successful execution followed by an application return-decoding error also never authorizes repeating the business action. Apply this through generated contract decoders and wallet call/deploy outcomes: distinguish known failed execution, unsupported execution, malformed response and successful execution whose present return bytes cannot be decoded by the application ABI. Missing required bytes are invalid response data; ABI decode failure is not execution failure or permission to repeat the action.

Rust and TS response readers must retain unknown tags and raw payloads as unsupported data; test extra nested fields on unknown variants. Rust `#[non_exhaustive]` alone does not change decoding. Serde internally tagged `other` accepts extra fields but discards tag/payload, so it does not meet the raw-preservation contract. Requests remain strict. Unknown backend XDR values are a separate boundary: preserve them only through an explicitly supported unknown result, otherwise backendDataInvalid/unsupportedValue. Never map them to success, absence or retry permission.

Wire decimal/base58 strings do not require string-only SDK values. Preserve existing byte-array and bigint ergonomics with validated codecs at the transport boundary.

Tradeoff: callers writing raw JSON make two kinds of reads. SDK users get a simple workflow, while the wire avoids a new composite lifecycle state machine in this release.

## Invocation simulation

```ts
type Invocation = {
  contract: Address;
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

## Events and root contract history

**Proposed:** separate initial and continuation forms, retain query predicates on every call, and make exhaustion explicit with `nextCursor: null`. This is the target contract; the backend correctness gates below remain implementation work.

```ts
type HeightAnchor = { kind: "atHeight"; blockHeight: Height };
type Start =
  | { order: "ascending"; anchor: { kind: "atBeginning" } | HeightAnchor }
  | { order: "descending"; anchor: { kind: "atEnd" } | HeightAnchor };

type PageRequest =
  | { start: Start; cursor?: never; limit?: U32 }
  | { cursor: Cursor; start?: never; limit?: U32 };

type TopicFilter = { topic0?: Hex[]; topic1?: Hex[]; topic2?: Hex[] };
type EventFilter = { contractAddresses: Address[]; topics?: TopicFilter };
type EventQuery = PageRequest & { filters: EventFilter[] };
type RootContractQuery = PageRequest & { contract: Address };

type EmittedEvent = { contractAddress: Address; topics: Hex[]; data: Hex };
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
      "contractAddresses": ["<base58-contract>"],
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

Continuation sends the same `filters`, the returned `cursor`, and optional `limit`; it omits `start`. The page size may change within the advertised bound; predicates and the historical walk may not. Bind cursors to the method family and decoded query values. Preserve ordered filter/address/topic arrays on continuation; JSON object property order and omitted-versus-empty wildcard topic positions do not change decoded identity. Canonical base58 is the only accepted identifier wire encoding. Do not require a new filter-normalization algorithm to accept reordered lists; changing their order can be rejected as queryMismatch. Exclude page size from the binding. Cursor direction and upper watermark remain backend-owned. The nullable `nextCursor`, initial/continuation exclusivity and validation rules below are proposed. Root-contract history substitutes the same `contract` predicate. Require exactly one of `start` or a nonempty cursor. Reject an explicit empty cursor to prevent accidental restart. Omitted limit uses the existing default of 50; reject zero and values above the advertised cap instead of retaining a magic zero-default convention.

Retain current topic semantics: OR across filters; contract membership AND topic conditions within each filter; OR within each topic position; AND across positions. An omitted topics object, omitted position or empty topic list is a wildcard. Normalize these equivalent forms before query binding; null is invalid. Each specified topic is exactly 32 bytes; there are at most three positions. Keep nonempty contract lists and the current filter/value bounds. Do not interpret an empty contract list as a full-chain scan.

An explicit `atHeight` anchor is inclusive when available. `atBeginning` is valid only ascending; `atEnd` only descending. Reject the opposite combinations as InvalidParams, rather than preserving today's surprising empty-page behavior. These boundaries refer to the retained serving range. An ascending explicit anchor beyond the available tip exhausts immediately; descending starts at the available tip. An explicit height below retained history should report unavailable history instead of silently clamping or exhausting; this requires the backend outcome described below.

The cursor fixes the walk's upper bound; it does not follow new blocks. The page's indexed bounds describe serving availability at response time, not a completed application checkpoint or the exact fixed cursor range. Keep paginating through empty event pages while `nextCursor` is non-null. The current backend reports oldest height zero; future pruning behavior must be qualified, not inferred from the field's presence.

An event's identity within a chain instance is `(transactionHash, sequenceNumber)`. For an ascending walk, the initial response's `latestIndexedBlockHeight` is the fixed upper watermark W encoded in that walk's continuation; preserve this existing backend relationship as an explicit contract. Retain W separately from later page bounds. Only after every page reaches `nextCursor: null` and all returned events have been processed may the application checkpoint through W. An empty completed range can checkpoint W too; an initial requested height above W does not advance a prior checkpoint. Restart the next walk inclusively at W and deduplicate the boundary. Persist application changes and its checkpoint together where needed. If any page fails or reports latestIndexedBlockHeight below W, do not mark the walk complete. The backend must reject continuation when current serving height is below cursor W as backendUnavailable, for both directions; it must not clamp W down and return false exhaustion. No cross-reset or pruning-proof durability is implied.

Persist checkpoints only for completed application ranges, replay their boundary and deduplicate. Receipt, event and scheduler retention differ. Do not use one page's oldest height as a global retention guarantee. Do not use `latestIndexedBlockHeight` as the last processed block.

Root history returns references only to transactions whose root invocation targets `contract`. It excludes internal invocations and addresses merely touched, debited or credited. It is not full wallet history, a transfer index, or a transaction-body fetch. SDK helper names must preserve this distinction.

### Backend correctness gates

The full announced application profile requires both history methods. Partial developer endpoints may omit them, but doing so for announcement requires explicit product re-scope and updates to CLI, explorer, smokes and PQ demo.

1. Invalid/malformed/unsupported cursors and predicate mismatch are typed protocol/indexer outcomes, mapped to invalidCursor/-32602. Binding includes method and canonical decoded query, excludes page size, remains backend-owned and fits the 256-byte cap. It prevents accidental mismatch, not malicious token forgery. A gateway must not decode cursor internals.
2. Check RocksDB iterator status, malformed in-range keys, every missing/unreadable/undecodable linked hydration row, out-of-range event position and malformed projection. Fail whole page with typed error and no successful cursor/exhaustion. Current atomic/no-pruning indexing gives no legitimate missing-linked-row exception. Cover events and root-contract history, plus malformed receipt/single/batch simulation events.
3. Keep block/emitter consistency checks but turn query-triggered CHECK failures into typed errors rather than process termination. Retain appropriate ingestion invariants. Test poison rows without losing service for other queries.
4. Reject regressed serving watermark below cursor W; do not clamp continuation. Test ascending/descending restored/replaced-indexer cases. SDK defensive walking must also refuse checkpoint advancement.
5. Report outsideRetention only with authoritative retained-range evidence. Current strict fresh origin requires epoch0, first height1 and contiguous later epochs, so oldest0 is genesis-inclusive and not a proven hidden gap. Qualify the strict fresh origin; do not invent non-genesis support or reset identity.

For continuation, the adapter supplies fixed valid legacy XDR order/anchor placeholders, such as Ascending/AtBeginning, when `start` is absent. The backend uses cursor-owned order/position/W and must ignore those placeholders for continuation/query binding. Test unchanged results with different valid placeholders. No binary layout change is required solely to omit `start`. Typed protocol errors/query binding remain final gates. Internal fail-closed patches can land first using existing failure carriers. Coordinate hogd#4454 (still skips missing rows) and#4458 (direct seek, not complete scan budgeting).

Add a positive configured examined-candidate cap per event page in slice 2. Resume exclusively after the last successfully validated visited position, including nonmatches, and emit a cursor only when candidates remain. Preserve query binding and W. Integrity/I/O errors fail the page rather than returning partial results or a checkpoint. Pair with #4458-equivalent direct seeking. Test both directions, interleaved contracts, overlapping filters, cap=1, selective/no-match initial and continuation pages, exact exhaustion and result-limit-before-candidate-limit. Empty nonterminal pages are a target shape; the current uncapped loop does not normally emit them.

General time/byte budgets, cancellation and durable pruning/reset/repair remain follow-ups. The cap bounds candidates, not RocksDB latency, repeated whole-log decoding, initial fanout or post-timeout work. Before announcement, qualify dense/selective/no-match queries and downstream work after timeout/abandonment on a fixed representative dataset against preselected numerical thresholds. Unacceptable results block announcement unless product scope is explicitly changed.

SDK history walking freezes predicates and initial W, handles empty nonterminal pages, deduplicates `(transactionHash, sequenceNumber)` and emits completedThroughBlockHeight only after processing terminal completion with every serving bound at least W. Ascending order is `(blockHeight, decoded transactionHash bytes, sequenceNumber)`; descending reverses it. Root references use height/hash. It is not base58 lexical order or execution/causal order.

Incoming-native-transfer recipe: query the native-token contract, canonical Transfer signature topic0 and destination address bytes as 32-byte hex topic2. Reuse generated typed topic helpers and test address-to-topic conversion. Generated signature leaves two indexed value positions. Transfer events are not full balance history. Freeze token event/error signature identity at 0.1 and `groundhog-erc20-if` for this release; a package 0.2/1 bump changes it even with the label pinned. Run the existing native/RV32 parity test with exact candidate pins. Independent event-version controls can follow.

Source verification and exact file anchors: [reconciliation HIST/EVT/SOL dispositions](review-evidence.md).

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
| Qualified indexer history | getEvents, getTransactionsByRootContract |
| View backend | simulateInvocation, simulateInvocations |
| Local scheduler | dev_getPendingTransactionCount |
| Hosted faucet REST adapter | faucet_requestAirdrop |
| Local submit/indexer + validated signer/journal/funding config | dev_fundAccount and local faucet_requestAirdrop |

Derive discovery after final module merging/removal and finalize it before serving. Pinned jsonrpsee 0.25.1 supports `remove_method`; no plane-trait split is required to work around its absence. An initialized-once immutable method list or register-only builder can implement late binding; the spike chooses the smallest fit. Configured outage is an error, not method removal; unconfigured plane is absent. Full announced profile requires scheduler/DB/indexer/view. Gateway startup must compare manifest protocol with linked codec protocol; the current override/intro-token mechanism does not establish compatibility.

Keep `wireFormat` as the compatibility authority. Release/implementation fields remain diagnostic; validated bundle identity retains its existing producer-owned format and provenance. Do not blindly re-encode Git commit IDs or artifact identifiers as chain hashes. Omit unavailable diagnostic fields consistently, including commitHash.

Keep `getVersionInfo` bootstrap stable across later wire majors. The native SDK has no old-name diagnostic fallback. A missing wireFormat, including a Stellar response to the same method name, is a typed incompatible/non-Thruput endpoint error. Add fixtures for Stellar transaction/base64 and ledger/filter requests yielding invalidParams; valid noarg {} remains valid.

Cache discovery per client/transport instance, not globally by URL. New instances validate anew. Mutable custom transports expose explicit invalidation. Incompatible responses, required-method -32601 or network mismatch invalidate/revalidate; never automatically replay a mutation after refresh. No imaginary HTTP reconnect event is required. Prohibit mixed incompatible contracts behind one endpoint. Wire range and required methods are checked before actions; composed flows preflight before signing. Always compare caller/provider expected networkHash; never adopt a changed identity silently.

Counter, swap and token-launchpad app proxies are endpoints: expose getVersionInfo/getNetworkInfo, advertise the intersection of upstream and allowed methods, accept named objects, preserve bounded typed error data and return -32601 for filtered methods. HTTP authentication/rate limiting is separate. Static SDK capabilities are expectations, not runtime truth; unsupported optional methods fail locally. Dynamic capability typing can follow. Test all three proxies independently; preserve Counter's submission policy and the other proxies' read-only policy. Shared envelope/discovery/error helpers are optional, not a requirement to merge their handlers.

The planning target is `>=13.0.0 <14.0.0`, conditional on the coordinated major and demonstrated minor-version reader compatibility. If GH-507 already consumes that major with a different contract, choose the next coordinated major. Diagnostic implementation/release versions never select codecs. A new optional request field or enum variant is not automatically supported by an older server in the same major: gate it by observed wire minor or raise the SDK's minimum required minor. If the caller explicitly requests an unsupported feature, fail locally as unsupported; never silently drop that constraint. Test new-client/old-server and old-client/new-server directions, including strict unknown-field rejection; `supportedMethods` alone does not negotiate parameter support.

The public `networkHash` binds signing identity. It does not currently prove a particular reset/instance or provide a genesis descriptor. Do not add invented `genesisHash`, `chainInstanceId`, protocol version or native network label. A required verified instance identifier would be useful, but needs a runtime/bootstrap contract and explicit reset behavior.

Tradeoff: five small information methods cost extra calls on first connection. They have different sources, cache rules and failure behavior. A metadata supermethod would conceal those differences and couple basic discovery to backend availability. An SDK can compose them while preserving each failure.

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

**Proposed scope:** defer substantive funding implementation and its richer result/error schemas. This release includes optional method naming, strict named parameters, proposed field/encoding changes, generated schemas and consumer migration. Keep narrow corrections that prevent false admission/execution claims. The optional names in the map are selected.

`faucet_requestAirdrop` takes `{ recipient, clientNonce }`; `dev_fundAccount` takes `{ recipient, rawAmount, clientNonce, faucet }`. `clientNonce` is 1..32 decoded bytes and must remain unchanged within the documented recovery scope. Local funding requires a positive canonical raw amount and the expected local faucet identity.

Conservative release baseline, preserving today's result semantics with field migration:

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

A pending hash identifies retained work, not admission. `succeeded` requires known successful committed execution; constrain its schema to `execution.result: "return"` and reject contradictory failed/unknown outcomes. Local funding returns submission/reconciliation evidence, including an already committed transaction whose execution failed; callers inspect the receipt before asserting successful payment. Preserve the exact retained signed bytes on existing success/error paths. Do not add the follow-up's committed/stopped or submitted/stopped variants now. The existing pending delay remains seconds in this conservative baseline; unifying result delay units can follow. Preserve existing producer error retry hints now: checked-convert retryAfterSeconds to retryAfterMs, retain service evidence and fail invalid/unrepresentable hints as backendDataInvalid. Do not discard a known policy minimum and fall back to an earlier retry. This is narrow adapter work for explicit caller retries and diagnostics, not permission for automatic hosted repeats or funding durability redesign.

Select one narrow optional-service error: `-32030`, `data.kind: "fundingAttemptStopped"`. Map known `airdrop_attempt_terminal` / `airdrop_transaction_failed` service codes, or an equivalent known local service stop, to it. It means the funding service stopped advancing this attempt. It does not establish admission, nonexecution or permission to replace. Carry bounded `serviceCode` and any existing transaction hash/execution evidence; do not manufacture missing evidence. Stop automatic funding resubmission; read-only reconciliation can continue. This small projection avoids representing a committed failure as admission rejection without requiring producer outcome enrichment.

Keep ordinary parameter, overload and backend errors in the common catalogue with available evidence. A service-policy refusal has no transaction admission field. Unknown service errors do not become known terminal stops; preserve their bounded cause under backendUnavailable. Use `-32031` for unresolved local funding with retained signed evidence. Existing outer wrappers must not erase a recognized stopped cause or turn it into automatic retry permission. The richer local state machine remains deferred. The concrete optional error data is:

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

Document the current hosted guarantee as same-store/retained-record only. Do not add stronger SDK automatic retry across unknown store continuity. Keep existing local request/evidence recovery through the migration, with the narrow stopped-cause rule above; this does not require fixing every pre-existing funding lifecycle issue. Deferred findings and candidate schemas are retained at the end of this note. Future breaking changes still need a wire-version decision.

```ts
// dev_getPendingTransactionCount({})
type PendingCount = { count: U64 };
```

The local pending count is this runtime's pool observation. It is not a network-wide count or the next account nonce. The hosted gateway lacks its implementation and must not advertise it. Future hosted support needs a deliberately scoped method.

Remove the current unsupported `groundhog_dev_getTransactionDetail` call/deploy probe, which runs beyond verbose mode, and use the fetched receipt. Preserve or move `dev_detail::format_event`, used by `view`; audit references, exported DTOs, tests and `thruput-trace` dependency before deleting unused types. A future `dev_getTransactionDetail` would take `{ transactionHash }`. Existing result types start with `{ receipt, gasConsumed, gasLimit?, storageDeltas, trace? }`, but those types do not establish a supported server contract. Current local registration has no method, and the CLI tolerates method-not-found. Deliver and qualify an implementation, define missing-versus-pruned behavior, then apply receipt field renames recursively and advertise the capability. This corrects the earlier plan's claim that it was served locally. [Local registration](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/node-supervisor/src/server.rs#L635), [developer types](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/dev.rs).

## Error shapes and recovery

**Proposed:** separate error cause, submission admission evidence and SDK recovery. Use stable typed `data.kind` and operation-specific details in the standard JSON-RPC envelope, and keep bounded recovery policy in the SDK. Messages are human diagnostics, not values to parse. Standard dispatcher-generated errors may lack typed data. New application errors carry typed data; generated per-method schemas must describe it. The admission/recovery rules, code allocations and optional reason enums below form the proposed contract, including the narrow -32030 service-stop code. Implementation is not complete. Only narrow funding contract corrections are in scope; the broader funding mappings are deferred.

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

Do not infer admission from the numeric code. `-32051` and `-32050` arise before and after dispatch. `-32053` includes transient scheduler refusal after dispatch: capacity, not-ready and stale-auth state. Retain overloaded as the family name but do not claim a narrower cause. InternalError maps to backendUnavailable with rejected admission, as protocol guarantees nonadmission; missing/failed root-account hog-age lookup and dependency failures are currently conflated, so do not invent insufficientFunds. Local expiry uses `-32001` before dispatch. Preserve the categories and fix the descriptions. Add typed context where directly known; return `unknown` elsewhere rather than building machinery just to claim more certainty.

Explicit current scheduler mapping: Admitted→success; AuthenticationRejected→-32001/rejected; Retryable→-32053/rejected; InternalError→-32050/rejected. Wrong reply cardinality, unknown reply values and transport loss after possible dispatch remain unknown. None settles an earlier attempt.

Carry typed phase/evidence through gateway and local submit batching and outer deadlines. Cover queue expiry, cancellation before dequeue, timeout during precheck, sink handoff, lost replies and wrong-cardinality outcomes. `notAttempted` requires proof that no worker can later dispatch this attempt; entering the sink alone does not prove either dispatch or non-dispatch. After possible dispatch use unknown unless stronger evidence arrives. Only attach `transactionHash` when validly computed; a precheck timeout before hashing can omit it. Outer deadlines must preserve already obtained evidence. Dispatcher errors and lost HTTP responses can lack both admission and hash, so the client must retain its own evidence. Do not claim all current generic errors already carry this information.

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

**Proposed:** keep existing applicable categories, fix the demonstrated middleware collision and allocate two new error families. The following contains the selected common/transaction catalogue plus optional -32030 funding stop and relocated -32031 unresolved funding. The numbers are a native catalogue, not an EIP-1474 promise; removing the misleading -32003 overlap does not make generic Ethereum error decoders safe. Substantive optional funding outcome/mapping changes are deferred; narrow truthful-contract corrections remain in scope. These decisions do not mark implementation complete.

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

The 14 October targeted review rechecked tools remote main at `d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99`. The matching lockfile pins jsonrpsee 0.25.1. Its `-32010` is oversized batch request and `-32011` is oversized batch response. The shared batch service actually emits the latter after dispatching members, so this is an active ambiguity, not just an overlapping constant. Move freshness to `-32012` and place unavailable history next to it at `-32013`.

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

Required work, not completed by agreeing to this schema:

- Replace string-only shared errors with typed cause/detail and directly known admission evidence. Preserve evidence through outer deadlines, batching and both gateway/local adapters; use unknown when dispatch cannot be ruled out.
- Produce reasons at validation and backend decoding sites. Keep messages diagnostic; do not classify errors by parsing them. Fix overload catalogue wording and the local expiry message that currently suggests re-signing without reconciling previous attempts.
- Add backend/protocol cursor validation, query binding and unavailable-history outcomes. Fail malformed event pages instead of advancing past dropped events. Include candidate-bounded event scans with advancing cursors and direct seeking; general byte/time budgeting and cancellation remain deferred. Qualify the deployment envelope.
- Generate all 12 application data kinds under 11 named numeric-code components from Rust. Components are already name-keyed; replace the numeric lookup's single-data-shape assumption with per-code unions, including invalidParams|invalidCursor for -32602 and operation-specific evidence. Do not list duplicate codes for one method or permit invalid cross-context combinations. Generate TS schemas/fixtures from the owner types. Test examples, unknown refinements, missing framework data, numeric collisions and gateway metrics classification. Qualify oversized individual and batch responses after writes, receipt lag and ambiguous submission recovery.
- Keep narrow optional funding corrections in scope: preserve available service causes/signed evidence, avoid false admission/execution claims, and document current recovery limits. The deferred follow-up contains producer enrichment, lifecycle redesign and durability work; do not make that work a release prerequisite.

Metrics labels must be bounded by the final registered method set plus an unknown bucket. Cover every method and selected error code, including freshness/history, funding, invalid backend data and individual/batch size errors. Keep framework/application origins distinct while old code collisions exist. Do not label with arbitrary request methods, messages or service payloads. Coordinate final label names with dashboards.

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

**Runtime prerequisite for submission announcement, even with SDK automatic retries disabled:** hogd#4468 or equivalent fixes nominated-cache duplicate behavior; #4471 supplies stacked full-capacity coverage. Raw callers can manually repeat an envelope, so disabling SDK retries does not waive duplicate-admission correctness or full-capacity coverage. Qualify nomination between scheduling chunks, full queue, abandonment/reenqueue and postcommit replay before enabling automatic exact-byte redispatch. Until then only proven never-dispatched attempts with no earlier uncertainty can be retried automatically. A later rejected attempt never clears earlier uncertainty.

After qualification, recovery requires identical approved bytes/hash, verified expected network, unchanged chain instance, unexpired validity, no known commitment, bounded attempts/backoff and continuing caller/provider permission. Receipt lag, failed reads or passed expiry do not satisfy a nonexecution precondition. Re-signing or modifying nonce/expiry is a new action requiring reconciliation/approval; a replacement/cancellation helper is deferred pending qualified loser semantics.

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

Retain immutable SubmissionEvidence before dispatch; submission/observation errors, including cancellation after signing, carry it plus strongest known commitment and original cause when available. Add local onSigned persistence hook before send, matching provider composition. Low-level explicit same-byte submission is available under verified identity/replay preconditions; never replay a consumed provider approval or silently bypass provider/session revocation. Optional locally known root gasLimit may be retained for diagnostics, never fabricated from receipt data or treated as proof of out-of-gas.

Retain exact signed bytes, body hash and expected network identity before the first send. A caller timeout/cancellation stops local waiting; it does not undo submission. A lost batch response leaves each dispatched submission independently uncertain. On retry, correlate batch members by JSON-RPC IDs, not response order. Retry only the appropriate members under their own policy.

Passing expiry prevents future inclusion but does not prove the transaction never committed. Updating nonce, expiry or signed envelope is a new action, not transport recovery. Preserve known commitment even if a later endpoint returns unknown; handle chain reset as an explicit identity/recovery event. A missing record outside retention cannot establish nonexecution.

**Proposed narrow SDK guard:** preserve the existing single-call `requestAirdropAttempt`. The high-level SDK `requestAirdrop` helper makes one `faucet_requestAirdrop` call on both hosted and local-node endpoints. For this major, do not automatically repeat that mutating RPC, including policy refusals and pending polls. Preserve hints for explicit caller retries; HTTP 429, serviceCode and retryAfter do not prove nonexecution. Add no funding-dispatch field for the optional retry optimization, and keep transaction admission separate. If pending includes a transactionHash, continue with read-only status/receipt polling within the caller budget and assert execution success. This is required on local nodes too: asynchronous block driving can leave the first response pending with a hash. A direct succeeded result completes normally after validation. An RPC error or pending without a hash ends automatic funding progression with the typed cause or unresolved/interrupted outcome, retaining the original recipient, nonce, expected network, last response and any observed hash/commitment. It does not trigger another faucet call. Preserve these on timeout, cancellation and terminal errors too. Show explicit nonce creation/persistence before first dispatch in recovery examples. Do not create a fresh nonce inside an automatic retry.

Automatic hosted repeats and any qualified-continuity policy remain follow-up work. An unchanged URL or earlier pending response is insufficient proof. This is an evidence/retry guard, not a new durability mechanism. Explicit local `dev_fundAccount` retry behavior stays scoped to its documented journal guarantees; it does not change the SDK single-call `faucet_requestAirdrop` composition. Keep the existing high-level success type: it may assemble `AirdropSucceeded` only from the retained pending response's recipient/amount/hash plus a matching committed receipt with `execution.result: "return"`. This is an SDK observation of success, not evidence that the service persisted its terminal state. A failed or unsupported receipt throws the corresponding typed outcome with the original request and full receipt. Missing evidence remains unresolved; do not invent it. Defer a new qualified-continuity configuration option with the stronger funding follow-up; the default conservative behavior needs no such option.

Expose explicit resume with the complete original recipient/network/nonce request and any evidence. Calling the nondurable hosted service again requires caller opt-in to its documented duplicate-test-funding risk; a later store miss cannot clear earlier uncertainty. Hosted faucet retries reuse the original recipient, network and nonce within a proven idempotency-retention scope. The current in-memory producer does not establish continuity across restart, independent replicas or terminal-record expiry; stronger hosted recovery is deferred, and the SDK must not imply that guarantee in this release. Local funding retries reuse the complete original request, including faucet identity and amount, and preserve retained signed evidence. Reconcile the original hash; do not generate a new funding nonce after an uncertain result.

Implementation must preserve evidence through outer deadlines, gateway/local adapters and SDK errors. The typed schema and generated error mappings need tests for pre-dispatch failure, authoritative refusal, ambiguous dispatch, absent error data, lost responses and commitment followed by receipt unavailability. This is contract work, not a cosmetic addition of `kind` fields.

## Final scope and delivery

The [delivery plan](../delivery-plan.md#eight-review-slices) owns component scope, eight slices and start/landing/release dependencies. Minimum history integrity includes typed cursor/query errors, iterator/key/hydration failures, process-safe consistency errors and regressed watermark rejection, plus receipt/simulation event integrity. These are required for the selected full product. Candidate cap/direct seek are in slice 2. General time/byte budgets, cancellation, durable history/reset recovery and substantive funding redesign remain follow-ups with measured public qualification gates.

New SDK versions target native API only. Keep response tolerance and strict parameterized requests. An output variant is minor only after supported readers demonstrate safe unsupported/raw fallback; otherwise major. Gate newly requested fields by minimum observed wire minor; never silently discard a requested constraint.

Use a manual stable-publication freeze while buildable slices land. Current manual workflows do not auto-publish on merge. One coordinated next major is owned with GH-506/507; 13 remains a planning target, not an independent reservation. Artifact dependencies are protocol/bindings/runtime → any required controlled CLI bootstrap generator → SDK → final tools/templates → packaging and matched React/apps/wallet/docs. The hosted path also requires matching Go bindings and chuckd metadata/manifest codecs before final packaging and startup qualification. Local candidate overrides do not prove hosted publication overrides; current npm publication has no isolated next channel.

Land slices, build the final set, deploy fresh devnet, qualify, announce. Reset/version Thruput stores and never auto-resubmit old evidence; preserve Stellar vault/keys. NetworkHash is not reset identity. No old endpoint, preparation window, dual-wire probes or rollback-state machinery is required. Native qualification gets a separate proposed evidence home linked to existing encoding/PQ work.

The implementation hold must be lifted explicitly before work starts. Shared-file owners/diffs, coordinated major, exact pins, artifact/release owner and qualification workload/numerical thresholds remain explicit readiness items. Review approval is not runtime qualification.

## Source baseline

Tools `d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99`, TS SDK `8ce62f4579273db51108edaab69096ed23aad436` and protocol `af043b5c1983c0c8b44683975a3c389369844e09` were rechecked unchanged. Hogd advanced from `7ca6a9d1a4c9fd5ac61d0a66d7a3790aaf3ad067` to `15d536912ebe70dae5a9b4b471440b928a6f365c`; the compared changes leave cited indexer/view/receipt code unchanged and update the protocol pointer to af043b5. GH-507/GH-506 remain In Progress, GH-428 Backlog and GH-413 Todo. GH-507 comments report unpublished implementation and partial validation; that code was not independently inspected. These are selected target contracts; no implementation or runtime qualification was performed.

- [Current API traits](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-api/src/lib.rs)
- [Status precedence](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/status.rs#L39)
- [Receipt projection](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/receipt.rs)
- [GH-507 encoding implementation](https://linear.app/grndhg/issue/GH-507)
- [GH-503 field classifications](https://linear.app/grndhg/issue/GH-503)

## Deferred funding follow-up: findings and candidate contract

**Deferred follow-up:** this section preserves findings and design candidates for a separate funding follow-up. None of its richer schemas, durability requirements or expanded qualification is a gate for the current RPC release. Reuse the proposed error catalogue where the semantics fit and put observable outcomes in results. Avoiding an extra service-error family depends on coordinated producer delivery in that follow-up; the gateway cannot recover missing facts from today's error messages.

For the follow-up, consider `pending`, `committed` and `stopped` for `faucet_requestAirdrop`. This would replace the release baseline's `succeeded` result. A committed result includes `blockHeight` and `execution`, whether execution succeeded, failed or uses a preserved unknown variant. The SDK's convenience flow checks execution success explicitly. This avoids a second success/failure classification that could disagree with the receipt.

```ts
type AttemptStop =
  | { reason: "transactionDropped";
      observedStatus: Extract<TransactionStatus, { status: "dropped" }> }
  | { reason: "admissionRejected";
      cause: { kind: "transactionRejected"; admission: "rejected";
               reason?: "authenticationRejected" } }
  | { reason?: never }; // the service knows it stopped, but has no finer evidence

type FutureAirdropAttempt = {
  recipient: Address;
  rawAmount: Amount; // original amount bound to this attempt
} & (
  | { attemptStatus: "pending"; transactionHash?: Hash; retryAfterMs?: U64 }
  | { attemptStatus: "committed"; transactionHash: Hash;
      blockHeight: Height; execution: ReceiptExecution }
  | { attemptStatus: "stopped"; transactionHash?: Hash; stop: AttemptStop }
);

type FundingEvidence = {
  recipient: Address;
  rawAmount: Amount;
  transactionHash: Hash;
  signedTransaction: Hex;
};
type FutureFundingSubmission = FundingEvidence & (
  | { attemptStatus: "submitted" }
  | { attemptStatus: "stopped"; stop: AttemptStop }
);
```

`stopped` means the funding service will not advance this logical attempt. It does not assert that its transaction never committed and does not authorize a replacement. Read-only reconciliation may continue. Require matching typed observations for specific stop reasons; omit the reason when only the service's terminal state is known. Known commitment wins over a later stopped observation. A current refusal must not erase previous uncertainty. The `admissionRejected` stop reason requires the identified transactionHash and describes one submission refusal, not the entire history of the logical request.

A pending hash identifies retained signed work, not admission. Hosted code stores the hash before broadcasting and can return it while executing or uncertain. Commitment without an available execution receipt can remain pending while preserving the known commitment internally; it must never trigger resubmission. Use one delay unit, `retryAfterMs`, on pending results and retry hints, as an optional decimal u64.

Keep `dev_fundAccount` as submission/reconciliation. `submitted` means authoritative admission or observed prior commitment, including committed failed execution; it promises neither current pool membership nor successful payment. Keep execution in `getTransactionReceipt`, with no inline receipt addition. The new stopped result represents processing observations such as Dropped (legacy Invalid has no selected native producer) without misusing the admission-rejection error. A direct proven submission rejection still uses `-32001`, with retained evidence and actual admission information. If previous dispatch remains unresolved, return `-32031` and preserve the latest refusal/observation rather than presenting it as the whole outcome.

### Follow-up service-to-RPC mappings

| Condition / current producer code | Mapping and client action |
| --- | --- |
| Invalid caller nonce/amount; wrong expected local faucet; same local identity with changed bound amount | `-32602` / `invalidParams`, existing field reasons and optional path. Wrong faucet is `invalidValue`; changed bound amount is `conflictingFields`. Stop automatic retries of the altered request; preserve the original request. A typed `detail` can identify the conflicting stored binding without exposing signing secrets. |
| `airdrop_invalid_client_nonce` after the gateway already validated and forwarded the request | Distinguish a demonstrated caller error from a producer/adapter contract mismatch. Do not automatically blame the caller for a correctly forwarded value rejected by an incompatible producer. Fix validation/serialization compatibility and qualify both boundaries. |
| Quota, recipient-pending limit, execution/attempt/journal capacity | `-32053` / `overloaded`. Preserve the provided delay and service code. Retry the same logical request within budget only when the retention/durability guarantee holds; do not choose a new nonce to bypass policy. No transaction `admission` is inferred. |
| Existing in-progress attempt; `airdrop_already_pending` race | Return pending only after obtaining the bound attempt record. A bare legacy error cannot supply its amount/hash; use overload plus service evidence while correcting the producer. HTTP 202 alone does not prove a successful result. |
| Failed committed receipt, currently folded into `airdrop_transaction_failed` | Faucet returns committed with the actual execution outcome. Local explicit funding returns submitted evidence; read receipt separately. Never `transactionRejected`. |
| Typed Dropped observation (legacy code also accepts unproduced Invalid), folded into `airdrop_transaction_failed` | Return stopped with the matching typed observation after commitment precedence checks. Stop automatic submission, preserve previous uncertainty, and do not invent admission evidence. |
| `airdrop_attempt_terminal` | Return the retained committed outcome if available; otherwise stopped with the actual terminal evidence. The producer must expose the original bound amount and retained hash/observation. A legacy terminal code cannot determine commitment. |
| Hosted network descriptor mismatch / `airdrop_network_mismatch` | `-32050` / `backendUnavailable`, with bounded service evidence. The gateway supplied the network fields, so this is service configuration failure, not a dapp input or transaction rejection. Stop blind retries against that mismatched service. |
| `airdrop_attempt_conflict` | This code means state changed/disappeared, not a caller payload conflict. Reconcile the attempt; if that cannot be done, `-32050` with service evidence. Do not restart it or change the client nonce. |
| Policy-store unavailable / persistence or connection failure | `-32050`; true timeout `-32051`. Preserve typed cause and any known retained transaction identity. These broad families do not prove every cause is transient. |
| `airdrop_submit_failed` | Enrich at producer: build/sign, send, observation failure, worker failure and timeout currently share this code. Use the evidenced family/phase; never infer admission or timeout from this string alone. Preserve an undifferentiated service failure under backendUnavailable until it can be classified. |
| Invalid retained state, prepared amount mismatch, malformed response, wrong returned recipient or impossible outcome combination | `-32054` / `backendDataInvalid` with the applicable proposed reason when the backend/service violates its data contract. A local construction bug can instead be `-32603`. Neither proves that earlier work did not execute. |
| Unknown well-formed service error | Preserve bounded service evidence; do not invent a transaction cause. Treat conservatively as backendUnavailable. An unsupported success shape that cannot be safely represented is backendDataInvalid. |

Preserve a bounded `serviceCode` string for faucet-origin errors. It records the producer's exact structured code for diagnosis and qualified method-specific handling; clients must not parse `message` or guess from HTTP status. Known network mismatch and state-conflict codes override a generic transient retry default. New structured producer causes should drive behavior; retain unfamiliar service codes without authorizing replacement.

This hint-preservation correction is also included in the narrow current release scope above. The producer already emits optional decimal `retryAfterSeconds` for quota, capacity and recipient-pending refusals. The gateway currently drops it. Validate and checked-convert seconds to milliseconds. If a supplied delay cannot be represented, fail the conversion as backendDataInvalid with retained service evidence; never discard it and fall back to an earlier retry. A client receiving a delay beyond its budget stops waiting. A missing hint uses bounded policy, not an invented server promise. Do not attach the transaction `admission` field to service-policy refusals. Only attach it where the producer supplies evidence about an identified transaction submission.

### Follow-up local unresolved evidence

Use `-32031` only when the local funding outcome remains unresolved and durable signed evidence is available. Keep the underlying typed cause/observation; signed bytes alone do not make every failure unresolved. For example:

```json
{
  "code": -32031,
  "message": "Local funding outcome unresolved",
  "data": {
    "kind": "localFundingUnresolved",
    "transactionHash": "<base58-transaction-hash>",
    "signedTransaction": "0x<original-signed-envelope>",
    "cause": { "kind": "backendTimeout" }
  }
}
```

`cause` is a nonrecursive typed cause variant, not a second exception stack. Include latest transaction/admission observations separately when actually known, and scope them to the identified submission. A local deadline can occur while persistence still holds the journal lock; evidence is then unavailable in that response. Return the underlying error, retain uncertainty and recover by repeating the exact original request. Do not fabricate evidence or extend the deadline just to obtain it.

Receipt/status observation errors must not discard a previously obtained submission cause. Retained expiry without an authoritative outcome stays unresolved; it is not permission to re-sign. SDK recovery must honor stopped/dropped observations even when earlier dispatch remains unresolved, so a generic unresolved wrapper cannot become unconditional retry.

### Follow-up idempotency and delivery requirements

The client nonce is an application idempotency key, distinct from the signed transaction's nonce. Local identity is `(recipient, clientNonce)` scoped by the journal's network and faucet identity; amount is immutable bound payload. Hosted identity includes networkId, genesisHash, recipient and clientNonce, with networkHash also checked against configuration. Reusing a nonce for a different recipient is a different request. Retry the full original request against the same verified service/network; do not rotate identity after an uncertain response.

For a no-amount faucet request, replay should use the amount already bound to the stored attempt even if operator configuration changes. New attempts use the new amount. The current local adapter expands the request with the current amount before journal lookup and can therefore reject an otherwise identical replay after a configuration change. Correct this lookup order. Hosted preparation also needs correction: a retained attempt without prepared bytes currently calls prepare_airdrop(recipient) using current configuration and validates against that current amount, not the stored attempt amount. Prepare from the bound amount or explicitly stop if the service can no longer honor it; never silently change the transfer. Explicit `dev_fundAccount.rawAmount` remains immutable and mismatches are caller errors.

**Material implementation gap:** default hosted production uses an in-memory policy store. Unresolved records survive expiry scans only while that store survives. Terminal records expire after the larger of recipient cooldown and 12 hours. A retry after record expiry, process restart or routing to an independent replica can create a new transaction with a different nonce/expiry. The current API exposes no reliable store-generation or retention boundary to let clients detect this. Reusing `clientNonce` alone does not establish safe recovery.

Recommend gating the advertised hosted recovery contract on durable storage shared by replicas, atomic attempt ownership, persisted original signed bytes before first send, and retained terminal outcomes/deduplication records. Do not silently turn an old key into a new transfer after terminal-record compaction. Retain at least the binding, hash and terminal outcome for the service's declared network-instance lifetime; retain exact signed bytes for unresolved work. At capacity, reject new attempts while allowing existing attempts to reconcile. Reset/instance migration policy remains explicit, not inferred from networkHash or a gateway boot ID. Until that follow-up is delivered, state the narrower same-store/retained-record scope and do not enable automatic recovery across unknown continuity boundaries.

The local persistent journal already retains request and signed bytes for chain lifetime, but does not persist commitment or terminal observations. Persist enough observations to keep known commitment authoritative across restart, or explicitly bound the server guarantee and require the SDK to retain it. Apply the same request identity across local facade methods so one method cannot accidentally create a second transfer for an existing attempt.

Required for the follow-up guarantee, not for this release:

- Extend faucet producer/policy records and REST payloads with typed attempt/receipt/status evidence. `airdrop_transaction_failed` and `airdrop_attempt_terminal` currently lose it; the gateway cannot reconstruct it from text.
- Implement shared optional-method result/error projections and regenerate schemas/SDKs. Preserve unknown execution variants. Fix the local blanket unresolved wrapper and reconcile previous sends before making a fresh refusal terminal.
- Carry service codes and retry hints through the gateway. Distinguish caller validation from gateway-authored network/configuration failures; retain hash before broadcast without claiming admission.
- Qualify concurrent same-key requests, lost final responses, restart, independent replicas, terminal retention/compaction, changed configured amount, failed committed execution, receipt lag, direct refusal after earlier uncertainty, expiry, and capacity that still permits reconciliation.

Source baseline: tools remote main rechecked at `d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99`. This was source review, not live/runtime qualification. [Hosted producer](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/airdrop.rs), [producer retry hints](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/error.rs), [default store](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/lib.rs#L91), [retention](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/faucet/src/policy.rs#L389), [local funding](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/node-supervisor/src/airdrop.rs#L334), [local journal](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/node-supervisor/src/funding_journal.rs#L30).
