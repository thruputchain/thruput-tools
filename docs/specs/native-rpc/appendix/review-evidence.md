# Appendix: source evidence and review dispositions

Review snapshot: 15 October 2026. Read the [API proposal](../api-proposal.md)
and [delivery plan](../delivery-plan.md) first. The
[contract appendix](contract-details.md) contains the detailed target shapes.

This appendix records all 65 finding IDs from the original independent review
and the [follow-up PR review](#follow-up-pr-review). The original summary stated 60; the raw findings
contained 65. Dispositions explain the proposal's choices and do not establish
team approval or runtime readiness. Implementation and Linear changes remain on
hold; this draft PR contains documentation only.

## Verification boundary

Three independent source-verification agents divided all 65 IDs (20 tools/Ethereum, 18 runtime/Solana, 27 consumers/Stellar/delivery). The plan editor reconciled conflicting recommendations, inspected the reports, source evidence and current Linear records, and selected the final changes. Origin revision/tree data and current source snapshots were matched; relevant PR heads and diffs were re-read. External convention claims were checked against primary sources listed below. No runtime correctness or performance test was run. A fresh read-only public version query confirms the baseline mismatch only. GH-506/507 unpublished work is reported evidence, not inspected implementation.

Deployment scope: internal-only devnet, disposable state. Final path: land slices, assemble one final artifact set, fresh deployment, qualification, announcement. A controlled bootstrap package can be a build prerequisite without becoming an intermediate hosted API.

## Original review disposition summary

| Disposition | Count |
| --- | ---: |
| accept | 13 |
| accept with change | 47 |
| reject | 3 |
| defer | 2 |

Accept with change means the verified concern is retained but its scope, diagnosis or proposed remedy is corrected. Defer includes a concrete follow-up below. A rejected diagnosis does not waive adjacent verified work.

Key corrections to the independent recommendations: HIST-4 is unsupported by strict fresh-origin rules; CR-11 is a substring-count artifact; DEL-2 is unnecessary for an internal-only disposable devnet. Batch simulation already rejects malformed events. ExecResult/root output are not guaranteed one-to-one. A token wire_crate pin alone does not freeze signature identity. Manual publication workflows do not publish on merge. Noarg null versus omission is lost before jsonrpsee handler dispatch.

## Current origin revisions

| Repository | Revision |
| --- | --- |
| [thruput-tools](https://github.com/thruputchain/thruput-tools/tree/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99) | `d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99` |
| [hogd](https://github.com/thruputchain/hogd/tree/15d536912ebe70dae5a9b4b471440b928a6f365c) | `15d536912ebe70dae5a9b4b471440b928a6f365c` |
| [thruput-protocol](https://github.com/thruputchain/thruput-protocol/tree/af043b5c1983c0c8b44683975a3c389369844e09) | `af043b5c1983c0c8b44683975a3c389369844e09` |
| [thruput-protocol-rs](https://github.com/thruputchain/thruput-protocol-rs/tree/18d7c01ee21ab2c0c5d74eb5262309de0c0a187a) | `18d7c01ee21ab2c0c5d74eb5262309de0c0a187a` |
| [thruput-protocol-ts](https://github.com/thruputchain/thruput-protocol-ts/tree/3ab9dbec41ac65ff89ef8c0552c76ec57f29ea42) | `3ab9dbec41ac65ff89ef8c0552c76ec57f29ea42` |
| [thruput-sdk-rs](https://github.com/thruputchain/thruput-sdk-rs/tree/12337492533acfa45a475cabc5af88ffe456559d) | `12337492533acfa45a475cabc5af88ffe456559d` |
| [thruput-sdk-ts](https://github.com/thruputchain/thruput-sdk-ts/tree/8ce62f4579273db51108edaab69096ed23aad436) | `8ce62f4579273db51108edaab69096ed23aad436` |
| [thruput-react](https://github.com/thruputchain/thruput-react/tree/5e32308be1a407f91c8237c60e1bae2f0fff2ade) | `5e32308be1a407f91c8237c60e1bae2f0fff2ade` |
| [freighter](https://github.com/thruputchain/freighter/tree/577cd7c0f8f94cda971b5ad0c30a596f8c43dd2c) | `577cd7c0f8f94cda971b5ad0c30a596f8c43dd2c` |
| [thruput-examples](https://github.com/thruputchain/thruput-examples/tree/c8ba8730d0d06e027dc939afb5974e32992f264f) | `c8ba8730d0d06e027dc939afb5974e32992f264f` |
| [thruput-explorer](https://github.com/thruputchain/thruput-explorer/tree/ec1b4650d36726816cbeeb56a3e9b0e9439970d2) | `ec1b4650d36726816cbeeb56a3e9b0e9439970d2` |
| [thruput-docs](https://github.com/thruputchain/thruput-docs/tree/8b3284919d0c6079894201ad3d68a34dbd39afa4) | `8b3284919d0c6079894201ad3d68a34dbd39afa4` |
| [agent-skills](https://github.com/thruputchain/agent-skills/tree/a976723d9f85d0d4f4b03e79e6f41728b012dc92) | `a976723d9f85d0d4f4b03e79e6f41728b012dc92` |
| [packaging](https://github.com/thruputchain/packaging/tree/73566ec94a6152b5bc4899b15e43ca005aa60b3c) | `73566ec94a6152b5bc4899b15e43ca005aa60b3c` |
| [ops](https://github.com/thruputchain/ops/tree/f03432c92a4f3af7375306a24bc7f680207c451b) | `f03432c92a4f3af7375306a24bc7f680207c451b` |

Source baseline: tools wire12/release0.30.0, SDK0.19.0. Public version read returned wire11.2.0/tools0.29.2/bundle0.1.31-rc.6/commit a2f492eaebf7d73f29773f78a4b75548bbec56e1. A manifest override can alter the handshake token without changing linked codecs; qualification/startup must compare them.

Current open PR heads: hogd#4468 ee25424ad896bf6ff219116ca47781102f6985d7; #4471 40236d4ccbc7e344e4949423408d0b650872050c; #4454 38d4a5d54b6771b32ddc64761c20ccbe561c10fd; #4458 83a3fc42af2420d7cd363e3901a12bff6dd4740f; tools#553 003996751e47fae130ad1933023d51a12d5fe725; Freighter#3 1163d8009651a8eacf9c974bfaa5556b035fbc1a; examples#134 e04feb3c397a332ab84fd7497a971f6378de9f5f; ops#230 4ae3a728ce47382751727d835f8b89072789fead. Other Tools overlap heads are under TT-9. Refresh before implementation.

## Primary external references

- [JSON-RPC2](https://www.jsonrpc.org/specification): structured object/array params and omission; selected noarg null normalization is a deliberate permissive exception, not a standard requirement.
- [EIP-1474](https://eips.ethereum.org/EIPS/eip-1474), [ethers provider](https://docs.ethers.org/v6/api/providers/jsonrpc/), [viem error source](https://github.com/wevm/viem/blob/main/src/errors/rpc.ts), [Ethereum topic schema](https://github.com/ethereum/execution-apis/blob/main/src/schemas/filter.yaml): signed versus signing verbs, numeric error conventions, params and topic wildcards. Native semantics remain explicit.
- [Stellar sendTransaction](https://developers.stellar.org/docs/data/apis/rpc/api-reference/methods/sendTransaction), [getEvents](https://developers.stellar.org/docs/data/apis/rpc/api-reference/methods/getEvents), [getVersionInfo](https://developers.stellar.org/docs/data/apis/rpc/api-reference/methods/getVersionInfo), [SDK preparation](https://stellar.github.io/js-stellar-sdk/reference/network-rpc/): signed envelope, history and preparation comparisons.
- [SEP-23](https://github.com/stellar/stellar-protocol/blob/master/ecosystem/sep-0023.md), [SAC](https://developers.stellar.org/docs/tokens/stellar-asset-contract), [Stellar transaction source](https://github.com/stellar/js-stellar-sdk/blob/main/src/base/transaction.ts), [base transaction hashing](https://github.com/stellar/js-stellar-sdk/blob/main/src/base/transaction_base.ts): typed StrKeys, chain-context limits and network-bound hash comparison. The evidence does not prove the review’s arbitrary undeployed-ID loss claim.
- [Solana accounts](https://solana.com/docs/core/accounts), [sendTransaction](https://solana.com/docs/rpc/http/sendtransaction), [signature status](https://solana.com/docs/rpc/http/getsignaturestatuses), [address signatures](https://solana.com/docs/rpc/http/getsignaturesforaddress), [confirmation guide](https://solana.com/developers/guides/advanced/confirmation): identifier, submission, history and expiry comparisons; no imported confirmation-level contract.
- [RocksDB iterator documentation](https://github.com/facebook/rocksdb/wiki/Iterator): invalid iterator must be distinguished from error by status.

Pinned dependency mechanics were checked in installed jsonrpsee 0.25.1 source against the tools lockfile: types/src/request.rs54, proc-macros render_server.rs396–456, render_client.rs230–271 and core traits.rs45–80. Raw-handler registration alone cannot distinguish omitted/null params. An upstream raw-source fetch failed; this mechanics claim rests on the exact pinned source, not an unverified online citation.

## Findings

### eth review

#### ETH-1: accept with change

**Reviewed claim:** Receipts and simulations also drop malformed events silently, but the no-silent-loss rule only covers history pages

**Disposition / plan change:** Adopt fail-closed projection with separate trusted observation evidence as TT2; no droppedEventCount success escape. This is explicit acceptance coverage and receipt tradeoff resolution, not proof every original plan sentence contradicted itself.

**Reverified evidence:**
- Same current-source evidence as TT2. Batch already rejects malformed events; only receipt and single simulation drop.

**Scope:** GA integrity slices3/5

#### ETH-2: accept with change

**Reviewed claim:** Simulation and receipt use the same 'result' tag with different values for the same outcomes; receipt 'reverted.message' is never emitted

**Disposition / plan change:** Take cheap in-major unified return/abort/fault current outcome vocabulary, retaining receipt execution vs simulation output parent fields and transaction-only legacy/unknown handling. Remove reverted.message and manufactured fault.kind. Apply consistency rules first; never derive transaction success solely from root frame. Unsupported results preserve raw typed value and never success.

**Reverified evidence:**
- tools types/receipt.rs:85-115 and view.rs:216-231 use two vocabularies; hogd-backend/receipt.rs:59-90 proves ExecResult and root frame are separate inputs, contrary to asserted 1:1 decoding.

**Scope:** GA wire shape slices1/3/4/5

#### ETH-3: accept with change

**Reviewed claim:** Retained -32003 'localFundingUnresolved' means the opposite of EIP-1474/viem -32003 'Transaction rejected'

**Disposition / plan change:** Move localFundingUnresolved to -32031 next to -32030; retire -32003 without reuse. State native catalogue is not EIP1474; SDK branches on typed kind and evidence, never assumes rejection from numeric code. This removes one misleading overlap, not generic EVM compatibility.

**Reverified evidence:**
- tools error.rs:16-25 assigns -32001 rejected and -32003 localFundingUnresolved. EIP1474 explicitly maps -32003 Transaction rejected and -32001 Resource not found; current viem rpc.ts:260-277 maps -32003 to TransactionRejectedRpcError.

**Scope:** Cheap in-major error migration slices1/3/4

#### ETH-4: defer

**Reviewed claim:** The plan warns against changing expiry but never documents the safe replacement: reuse the same (root, nonce, expiry)

**Disposition / plan change:** Document no Ethereum replacement/cancellation guarantee and no automatic re-sign/rebuild. Follow-up: qualify same-root/same-nonce/same-expiry competing transactions across success/abort/fault and observe loser outcome before publishing a replacement helper. Source supports shared replay exclusion; it does not supply a distinct reliable cancellation/replacement outcome. User-authorized replacement remains separate and not a GA helper.

**Reverified evidence:**
- protocol transaction.x:108-138 defines root+nonce+expiry replay scope; hogd runtime/syscall_handler.cc:612-625 reserves nonce with body lock and :684-699 keys at root; :726-740 consumes nonce for discarded body. SDK prepareTransaction.ts:20-27,59-72 permits explicit nonce/expiry.

**Scope:** Follow-up SDK/runtime qualification; current docs safety note

#### ETH-5: accept with change

**Reviewed claim:** Rejecting `params: []` for no-argument methods, including bootstrap getVersionInfo, breaks common client and probe defaults

**Disposition / plan change:** Allow [] for all genuinely noarg methods alongside canonical {} and omission. Parameterized methods remain named-object-only, including an empty-array rejection. Resolve noarg top-level null handling as TT1 notes rather than promising impossible raw-handler discrimination. Not a legacy wire bridge.

**Reverified evidence:**
- sdk-ts transport/http.ts:161-174 emits params??[]; ethers official v6 JsonRpcApiProvider.send accepts Array or Record; JSON-RPC2 params may be omitted and structured values are object/array.

**Scope:** Cheap noarg DX plus TT1 feasibility

#### ETH-6: accept with change

**Reviewed claim:** Addresses inside event topics stay hex while every other address is base58; topic filter needs all three positions

**Disposition / plan change:** Keep raw 32-byte hex topic values, including addresses, and reuse existing generated typed topic helpers. Select optional topics object and optional topic positions; omitted and empty mean wildcard, explicit null is invalid. Normalize these equivalent forms before cursor query binding. Preserve array order. Explain that generated signature topic0 leaves two indexed fields. This is a small in-major DX fix, independent of EVT-1 signature stability.

**Reverified evidence:**
- tools types/events.rs:199-228 requires three topic positions currently, 32-byte Topic. SDK-RS erc20/events.rs:18-27,50-59 uses signature/from/to; SDK-TS contract/event.ts:121,153,245,380 resolves and decodes address topics already. Ethereum official filter.yaml allows wildcard/missing topics.

**Scope:** GA topic-request DX/docs, slices1/3/4; separate from EVT-1 signature stability.

#### ETH-7: accept

**Reviewed claim:** `sendTransaction` for signed bytes reverses Ethereum's meaning and differs from the SDK's own provider `submitTransaction`

**Disposition / plan change:** Keep proposed native sendTransaction RPC and explicit signedTransaction parameter; state it never signs or asks wallet to sign. Explain Ethereum contrast. Provider submitTransaction remains separate permissioned wallet interface; do not add renaming churn solely for verb symmetry.

**Reverified evidence:**
- EIP1474 sendTransaction creates/signs/sends while sendRawTransaction broadcasts signed data; Stellar official sendTransaction sends signed envelope. SDK-TS provider/types.ts:68-75 calls broadcast submitTransaction.

**Scope:** Docs only

#### ETH-8: accept with change

**Reviewed claim:** `assertSuccessfulExecution` is presented as new, but the SDK already has `assertTransactionSuccess` and `waitForExecution`

**Disposition / plan change:** Preserve existing assertTransactionSuccess and waitForExecution names and composition, as CR-8 recommends. Evolve the single assertion implementation for nested execution, full failed receipts and typed unsupported outcomes. Do not add assertSuccessfulExecution or require a return-value change merely for symmetry. Existing helpers make this a targeted migration, not a new wallet workflow.

**Reverified evidence:**
- SDK-TS wallet/execution.ts:28-31 assertTransactionSuccess is void; :83-90 waitForExecution composes wait/assert; wallet/client.ts:409 calls assertion.

**Scope:** SDK slice5 explicit migration

#### ETH-9: accept

**Reviewed claim:** `assumedAuth.nonce` is the account-derivation nonce but reads like a transaction nonce

**Disposition / plan change:** Rename assumedAuth pubkeySignature.nonce to accountNonce in native request and all examples/schema/SDK normalizers. It is not transaction uniquifier nonce.

**Reverified evidence:**
- tools types/view.rs:25-32 and48-52 nonce maps into PubkeyAccountPreimage.nonce; SDK-TS provider/types.ts:17-18 explicitly calls it account-derivation nonce.

**Scope:** Cheap in-major field fix slices1/3/4

#### ETH-10: accept with change

**Reviewed claim:** Onboarding must state that getBalance returns the unlocked balance and that gas is sized from the exact root invocation

**Disposition / plan change:** State getBalance excludes locked storage deposits. Simulate exact wallet-wrapped root with matching assumedAuth and account nonce; attached contract validation adds costs, no guaranteed gas estimator. SDK can retain supplied root gasLimit as local context in execution errors where already available, optional and never fabricated by receipt-only API. Do not diagnose out-of-gas from equality alone.

**Reverified evidence:**
- tools server.rs:458-476 reads native balance storage; current docs storage-economics.md:55 says unlocked/free. types/view.rs:205-212 limits gas equivalence to same root invocation/no attachments; SDK wallet/execution.ts:154-169 does not estimate wallet overhead.

**Scope:** GA docs; optional local SDK context small followup

### sol review

#### SOL-1: accept with change

**Reviewed claim:** Status/receipt cannot tell 'not committed' from 'not yet indexed'; a scheduler non-commit answer wins over an indexer miss or failure, so there is no Thruput equivalent of lastValidBlockHeight

The concrete issue is masked indexer failure plus non-commit scheduler outcome, and ambiguous retry/dropped evidence. The plan already refuses to infer nonexecution from expiry. Solana expiration guarantees no future processing, not that a previously lost response proves nonexecution.

**Disposition / plan change:** Apply STAT-1 and explicitly say retry/dropped/unknown/null are not historical noncommitment proof. No automated replacement or expiry refresh based on them. Do not add indexedThroughBlockHeight or expiredUncommitted as GA promises; a sound proof needs retained complete index coverage and race/expiry semantics.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/status.rs#L39-L58)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.h#L33)
- [Source 3](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L449-L458)
- [Source 4](https://github.com/thruputchain/thruput-sdk-ts/blob/8ce62f4579273db51108edaab69096ed23aad436/src/actions/public/waitForTransactionReceipt.ts#L194-L200)
- [Source 5](https://solana.com/developers/cookbook/transactions/confirmation)

**Follow-up:** Design a qualified noncommitment evidence API/SDK proof using indexing and retention bounds; do not imply Solana lastValidBlockHeight alone supplies it.

#### SOL-2: accept with change

**Reviewed claim:** Continuation silently clamps to min(current watermark, cursor W), which breaks the selected 'checkpoint W after nextCursor:null' recipe when a lower-watermark indexer serves the walk

Same as HIST-1. Lower bound is detectable from response; trigger is rebuild/restore/endpoint inconsistency rather than normal monotonic DB operation.

**Disposition / plan change:** Apply HIST-1 backend continuation check and defensive SDK W checkpoint rule; do not add cross-reset durability.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1989-L2030)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2387-L2406)

#### SOL-3: accept

**Reviewed claim:** History integrity gate misses two silent-exhaustion paths: RocksDB iterator errors and malformed index keys end the walk as normal end-of-data

Invalid iterator and malformed key collapse to exhaustion for both history methods. RocksDB documentation explicitly requires checking status when Valid becomes false.

**Disposition / plan change:** Apply HIST-2: fault injection must verify no successful exhaustion/cursor on iterator error or corrupt in-range key.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1843-L1923)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2433-L2464)
- [Source 3](https://github.com/facebook/rocksdb/wiki/Iterator)

#### SOL-4: accept with change

**Reviewed claim:** Unprefixed 32-byte base58 makes Ed25519 public key, pubkeyHash, account address, tx hash, block hash and network hash look the same; Solana developers will assume base58(pubkey) is the address

Protocol domain separates pubkey, PublicKeyHash and account address. Same32-byte base58 wire representation makes accidental substitution possible. Solana keypair accounts use public keys but PDAs are an exception, so avoid claiming all Solana addresses equal keys.

**Disposition / plan change:** Use distinct semantic types at SDK boundaries and explicit address derivation; add shared key/hash/address vectors and Solana onboarding callout. Preserve natural byte APIs through typed wrappers/constructors, no implicit pubkey-to-address conversion. UI heuristics or changed display defaults are not required for GA.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/preimage.x#L19-L23)
- [Source 2](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/types.x#L79-L84)
- [Source 3](https://solana.com/docs/core/accounts)
- [Source 4](https://solana.com/docs/rpc/http/sendtransaction)

**Follow-up:** Wallet/faucet known-key recipient warnings can be evaluated separately; do not promise arbitrary valid32-byte mistakes can be recognized.

#### SOL-5: accept with change

**Reviewed claim:** No supported recipe for incoming transfers: root-only history excludes credits, and the alternative (native-token Transfer events filtered by topic) is undocumented and hits the deferred unbounded scan

Root invocation history is not credits/all-account participation. Native Transfer topics provide a concrete incoming-transfer recipe; topics filter after scanning native contract stream. Solana getSignaturesForAddress is broader accountKeys history.

**Disposition / plan change:** Add incoming-native-transfer docs recipe using native token contract, canonical Transfer topic0 and destination raw 32-byte hex topic2. Reuse/test existing generated topic helpers and expose a small semantic address-to-topic convenience only if missing. Include selective/no-match native-token workload. This is not full balance history or a new indexed wallet-history API.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/transaction.rs#L326-L331)
- [Source 2](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/genesis.x#L35-L36)
- [Source 3](https://github.com/thruputchain/thruput-sdk-rs/blob/12337492533acfa45a475cabc5af88ffe456559d/standard-interfaces/thruput-erc20/src/events.rs#L17-L65)
- [Source 4](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2152-L2162)
- [Source 5](https://solana.com/docs/rpc/http/getsignaturesforaddress)

**Follow-up:** Indexed participant/transfer history, general time/byte budgets and cancellation remain follow-up. The follow-up PR review adds candidate cap/direct seek to slice 2.

#### SOL-6: accept

**Reviewed claim:** TransactionStatus `invalid` has no producer and carries a free-text reason

Public status Invalid exists, but current scheduler mapping only produces pending/committed/retry/dropped and indexer only committed/recentFailure. No current production source established for Invalid; free-text reason would become a needless frozen recovery branch.

**Disposition / plan change:** Remove invalid from selected new GA status union and active SDK/funding recovery examples; unknown future tags remain unsupported/raw. Migrate old internal consumers as part of coordinated breaking release, no compatibility endpoint required.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/transaction.rs#L25-L40)
- [Source 2](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/scheduler.rs#L65-L99)
- [Source 3](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/status.rs#L39-L58)

#### SOL-7: accept

**Reviewed claim:** Intra-block order of history walks is lexicographic by transaction hash, not execution order

Merge Position is block, hash bytes, sequence. Ordering within a block is hash order, not execution/causal order. Base58 lexical ordering does not generally match byte ordering.

**Disposition / plan change:** Document ascending (blockHeight, transactionHash decoded bytes, sequenceNumber) and reverse for descending; root history uses height/hash. Do not instruct clients to sort the encoded base58 strings or reconstruct causal order from pages.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1778-L1787)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2094-L2099)
- [Source 3](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2438-L2451)

### xlm review

#### STL-1: accept with change

**Reviewed claim:** Plan does not decide whether the RPC wire accepts C StrKeys in Address fields; the notes say both yes and no

Shared notes leave the input boundary ambiguous; current origin exposes separate typed human-input parsers. No inspected published branch proves the new wrappers.

**Disposition / plan change:** Decide canonical case-sensitive base58 only for typed RPC/provider identifiers; opaque payloads remain canonical hex. C/G/S accepted only in explicit typed human-facing SDK/CLI/wallet input helpers, then converted to bytes/canonical wire. G is a key, not an account address. Delete equivalent-wire-encoding clause; cursor binding uses canonical decoded identities. Coordinate with GH-503/507 without treating unpublished claims as reviewed code.

**Reverified evidence:**
- tools@d0a8cdc9 crates/signing/src/strkey.rs:35-82; crates/client/src/codec.rs:328-338
- Current GH-502/506/507 and GH-507 comments: RPC Base58<T> wrappers versus human input C/G/S reported separately

**Scope:** GA

#### STL-2: defer

**Reviewed claim:** A Thruput C address carries no network, and Freighter's own Stellar Send flow accepts C recipients as Soroban transfers

C representation is not chain-scoped and wallet routing supports the confusion risk. Exact irreversible-loss claim for arbitrary undeployed IDs is not established by this evidence.

**Disposition / plan change:** Keep canonical base58 copy/QR as announcement default. Track optional Stellar-display/copy/QR guards under GH-511: explicit network context and cross-chain paste tests before enabling. Do not invent a new thruput URI standard in the RPC plan. If optional C copy is enabled for announcement, its safety qualification becomes a release gate; otherwise defer it.

**Reverified evidence:**
- freighter@577cd7c0 extension/src/popup/helpers/soroban.ts:126-128; extension/specs/THRUPUT_WALLET.md:89-90
- Stellar SAC docs https://developers.stellar.org/docs/tokens/stellar-asset-contract (contract balances created on successful transfer; no proven undeployed-ID outcome here)
- Current GH-511 optional display copy/QR acceptance

**Scope:** wallet display follow-up

#### STL-3: accept with change

**Reviewed claim:** getEvents checkpointing relies on the client keeping the first page's latestIndexedBlockHeight, which conflicts with Stellar cursor and latestLedger habits

The plan already documents the initial watermark trap; SDK helper is a cheap DX guard. Crucially a receding indexer watermark must not falsely complete the walk.

**Disposition / plan change:** Ship bounded SDK history walk helper that retains initial W, freezes predicates, continues empty nonterminal pages, dedupes event identity, and returns completedThroughBlockHeight only after successful terminal completion. Backend rejects continuation when current indexed height<W; SDK also refuses checkpoint on lower bounds. No durable across-reset guarantee or new wire end-bound field required for GA.

**Reverified evidence:**
- companion history W recipe; protocol/indexer current cursor fixes walk maximum
- Stellar getEvents https://developers.stellar.org/docs/data/apis/rpc/api-reference/methods/getEvents (top-level cursor/latestLedger and id dedupe)
- runtime reviewer reverified current hogd: continuation uses min(current,W)

**Scope:** GA helper/history integrity

#### STL-4: accept with change

**Reviewed claim:** Shared Stellar method names are safe only if strict validation rejects Stellar-shaped traffic; no fixture proves this

Already fail-closed by wire validation in intended contract; add precise endpoint diagnostics and negative examples, not a new compatibility layer. A no-parameter call with {} remains valid if selected strict policy allows it.

**Disposition / plan change:** Add fixtures for Stellar getVersionInfo response missing wireFormat -> typed non-Thruput endpoint; Stellar transaction/base64 and ledger/filter objects -> invalidParams. Nonempty unexpected params fail; do not incorrectly reject valid empty no-arg params. GH-412 health/info mapping is an adapter follow-up, not speculative RPC GA work.

**Reverified evidence:**
- freighter@577cd7c0 extension/src/background/thruput/network.ts:22-34
- Stellar getVersionInfo https://developers.stellar.org/docs/data/apis/rpc/api-reference/methods/getVersionInfo
- Stellar sendTransaction/getEvents official API pages

**Scope:** GA fixtures; adapter follow-up

#### STL-5: accept

**Reviewed claim:** transactionHash is not network-scoped, unlike Stellar's hash; the contract does not say so

Thruput body hash omits network; signature preimage binds it. Stellar comparison independently verified.

**Disposition / plan change:** State hash is neither network- nor chain-instance-scoped. SDK/wallet/explorer records use verified networkHash + transactionHash and invalidate previous reset generation. Do not imply networkHash alone survives reset safely.

**Reverified evidence:**
- protocol@af043b5 transaction.x:153-172,198-217; tools@d0a8cdc9 crates/signing/src/build.rs:262-279
- Stellar official source https://github.com/stellar/js-stellar-sdk/blob/main/src/base/transaction.ts#L264 (signatureBase networkId) and transaction_base.ts#L291 (hash signatureBase), re-read via GH API

**Scope:** GA docs/cache criteria

#### STL-6: accept with change

**Reviewed claim:** No 'coming from Stellar' RPC mapping; the existing SDK export prepareTransaction implies Stellar's simulate-and-assemble step

RPC method ban does not ban the existing SDK helper. Existing helper only assembles body/nonce/expiry, never simulation or gas sizing.

**Disposition / plan change:** Keep SDK prepareTransaction name with explicit TSDoc and usage example; no needless rename to buildTransaction (SDK already has createTransaction). Add short Stellar-to-Thruput RPC mapping and explicit root gas budget/headroom, no footprint. Say addresses can be rendered C in optional human display; NOT all wire accounts are C, since canonical wire is base58.

**Reverified evidence:**
- sdk-ts@8ce62f4 src/actions/wallet/prepareTransaction.ts:19-82; actions/wallet/index.ts:3
- docs@8b32849 src/content/docs/ecosystem/stellar.md headings lack RPC mapping
- Stellar SDK https://stellar.github.io/js-stellar-sdk/reference/network-rpc/ prepareTransaction simulates and applies auth/footprint/resource fee

**Scope:** GA docs

### tools review

#### TT-1: accept with change

**Reviewed claim:** jsonrpsee #[rpc] and the open_rpc macro cannot produce or consume the selected flat, strict named-object shape. Server, generated Rust client and generator all need a dispatch decision.

**Disposition / plan change:** Select strict request DTOs per method with denied unknown fields and omitted-not-null optional field deserializers; custom shared raw-Params registration wrapper; typed Rust client request serialization implementing ToRpcParams with no wrapper key; generator expands the same DTO schema into by-name descriptors and whole-object x-params-schema for exclusivity. Preserve backend traits. Do not fork jsonrpsee or leave two independent schemas. Noarg canonical {} with omitted and [] accepted; explicit null is accepted as noarg-only normalization because raw Params cannot distinguish it from omitted; document this narrow permissive exception, avoiding new raw HTTP/WS envelope infrastructure; parameterized methods still require object. Resize slice1 L; generated Rust integration template is a consumer. Noarg wrapper checks only empty object/empty array/absence before dispatch and rejects {bogus:1} and [1]. Feasibility spike verifies server/local parity, typed client omission/null behavior and generated schema before broad migration.

**Reverified evidence:**
- jsonrpsee 0.25.1 render_server.rs:396-456 creates alias-tolerant map structs and accepts arrays; render_client.rs:230-271 inserts every fn argument into ObjectParams; tools open-rpc-macros/src/lib.rs:136-155 describes Rust arguments. Request.params is Option<Cow<RawValue>> (jsonrpsee-types request.rs:54), so explicit top-level null and absence are collapsed before Params callbacks. ToRpcParams (core/traits.rs:45-80) allows a typed serialized request object.

**Scope:** GA design, slices1/3/6

#### TT-2: accept with change

**Reviewed claim:** Malformed events are also silently dropped from receipts and simulation results, not only from getEvents pages

**Disposition / plan change:** Explicitly fail malformed receipt, single simulation, batch simulation and history event projection with backendDataInvalid/invalidEncoding. Preserve independently trusted receipt hash/commitment height and, when decoded and consistent, execution/gas as a separate typed observation in error data; no partial receipt or successful missing events. Unknown/contradictory execution data must not enter observation. SDK preserves this evidence and does not resubmit. Add gateway/local parity fixtures. Correct reviewer batch claim.

**Reverified evidence:**
- tools hogd-backend/event.rs:24-32 and receipt.rs:27-48 drop malformed events; single view.rs:207-220 drops; batch view.rs:80-86 ALREADY fails as BackendUnavailable. Gateway handlers/receipt.rs:110 and view.rs:140 meter; local read_backend.rs:526 and view_backend.rs:162 warn.

**Scope:** GA integrity slices2/3/5

#### TT-3: accept

**Reviewed claim:** Permanent configuration gaps are currently exposed as registered stubs or perpetual backendUnavailable, beyond the block/count stubs

**Disposition / plan change:** Use the configuration table in the contract appendix, Discovery and configuration. Register only supported configured planes, freeze sorted method_names after extension merge and provide that list to discovery. Allow explicitly partial developer endpoints; SDK composed flows capability-check before signing. The fresh announced devnet requires full core profile and qualified history. Outages do not change support.

**Reverified evidence:**
- tools gateway main.rs:291-299 unconditionally registers core, node-supervisor/server.rs:400-480 selects full/storage-only/scheduler-only and :635-646 merges core before optional modules; read_backend.rs:201-237 storage-only stubs; rpc_backend.rs:218-221 needs DB; gateway blocks.rs:15-21 uses DbGateway tip coalescer.

**Scope:** GA registration slice3

#### TT-4: accept with change

**Reviewed claim:** Hosted faucet service is itself a JSON-RPC client of the public gateway and must switch in lockstep

**Disposition / plan change:** Add hosted faucet RPC client and telemetry as slice6 consumer and exact fresh-devnet artifact set. No old-attempt draining or transition window: the confirmed deployment context has no state to retain. New devnet attempts still have documented restart/durability limits.

**Reverified evidence:**
- tools faucet/src/airdrop.rs:13,1071-1075,1140,1155 uses typed RPC client, chain tip, submission, receipt polling; telemetry.rs:296-324 names groundhog_sendRawTransaction; faucet/src/lib.rs:97 defaults in-memory store.

**Scope:** GA consumer slices6/8

#### TT-5: accept

**Reviewed claim:** For runtime-manifest deployments the startup handshake uses the manifest's protocol, not the linked one

**Disposition / plan change:** Correct startup claim. Require configured manifest protocol to equal the linked codec protocol for launch artifact qualification; successful intro alone proves only acceptance of supplied token. Prefer startup hard check in slice3; do not use override as codec compatibility. No legacy-runtime bridge needed.

**Reverified evidence:**
- tools gateway/config.rs:503-510 copies manifest.protocol into override; :540-554 compares only explicit settings; context.rs:244-252 chooses override before linked PROTOCOL_VERSION.

**Scope:** GA compatibility validation slices3/8

#### TT-6: accept with change

**Reviewed claim:** Receipt execution projection: 'kind' is adapter-manufactured, and buffers are not tied to the outcome

**Disposition / plan change:** With ETH2 normalize current receipt execution to return/abort/fault and remove invented kind/message. Before nesting validate ExecResult/root-output consistency. Impossible mismatch is backendDataInvalid/inconsistentResult and retains only independent trustworthy commitment evidence. The source allows contradictory pairs structurally; no evidence observed producer emits them.

**Reverified evidence:**
- tools hogd-backend/receipt.rs:59-75 maps ExecResult independently of :85-90 root frame buffers; deterministic kind is fixed fault; Reverted.message always None.

**Scope:** GA projection slice3

#### TT-7: accept with change

**Reviewed claim:** Batch-limit errors are already mislabelled in gateway metrics, and open PR #394 builds on that mapping

**Disposition / plan change:** Correct framework versus application metric classifications alongside catalogue move to -32012 and method registration labels. It is NOT safe to globally reinterpret -32011 before the major: real old view errors still have that number. An independent earlier metrics patch must distinguish origin/layer. Coordinate PR394; no implementation while held.

**Reverified evidence:**
- tools types/error.rs:265-277 maps -32010 to not_found and -32011 to view_height_unavailable; jsonrpsee 0.25.1 uses same numbers for batch body size; open PR394@7b3bca7ba54f1e20773f787bdd55ecc1f4b64a57 uses outcome_for_code in batch layer and old crate names.

**Scope:** GA bounded correctness slice3/8; can prepare independently

#### TT-8: accept with change

**Reviewed claim:** Per-method error mappings already exist as a hand-maintained table keyed on prefixed names

**Disposition / plan change:** Keep this Rust table as owned generator input, update all names and retired codes, cover every typed error data shape; assert registered method set equals documented set. It is not a forbidden hand-edited parallel JSON schema, and current panic already catches undocumented names.

**Reverified evidence:**
- tools open-rpc-spec/src/lib.rs:111-180 owns literal groundhog_* error table and panics on missing name; :92-109 has three typed x-data-schema cases.

**Scope:** GA generator slice1

#### TT-9: accept with change

**Reviewed claim:** Overlapping open PRs and a published competing StrKey branch are not recorded

**Disposition / plan change:** List overlaps. PR187 is design input, not implicit block-method scope. Old StrKey branch is not GH507 and must be reconciled against selected canonical encoding. PR292 body/transport limits,293 error redaction,394 metrics and469 CLI build specs need current-base compatibility review; useful independent changes need not wait for native API if behavior compatible. No claim local GH507 commits inspected. Replace old no-intermediate-publication logic with DEL3 build-one-qualified-announcement rule.

**Reverified evidence:**
- Current GH API: open draft PR187@67719be632534b2eb7200a59e25566f259b01143,292@488392e6330abeb10f56e3e27ecc58043cfccdc8,293@53beb286d46892ebf6ac44421ecfb9d91bbe75e8,469@1f4a07770154ae4530a4215ce3227cff8fd3bde1; open nondraft394@7b3bca7b and553@003996751e47fae130ad1933023d51a12d5fe725. render-addresses-as-strkeys branch f70f5d968d34ccda1a9a7d9c96f204719211bae5 still exists; includes strkey.rs. PR file diffs inspected.

**Scope:** Sequencing inventory

#### TT-10: accept with change

**Reviewed claim:** Scheduler 'Retryable' covers stale-block-height auth as well as capacity, but is projected as 'overloaded'

**Disposition / plan change:** Describe -32053 overloaded as transient admission refusal encompassing capacity, scheduler not ready and stale auth state. Do not invent reason distinction. Scheduler explicit Retryable rejects this attempt; a future protocol outcome split is follow-up. Exact-byte retry still needs same-chain-instance, unexpired, noncommitted preconditions. Retain overloaded as the selected family name and broaden its documented meaning; a public reason enum waits for a typed protocol split.

**Reverified evidence:**
- hogd rpc_from_coord.cc:184-185 maps stale auth height to Retryable; :430-434 readiness and :543-544 capacity also produce Retryable. tools hogd-backend/scheduler.rs:34-36 maps to Overloaded.

**Scope:** GA docs and evidence mapping; reason refinement followup

### hogd review

#### HIST-1: accept with change

**Reviewed claim:** Continuation silently clamps to a lower serving watermark and reports exhaustion, which breaks the W checkpoint recipe

Continuation clamps its fixed walk watermark W to the current serving watermark. This can falsely exhaust after same-chain indexer restoration/rebuild; it does not regress during an ordinary continuous durable indexer epoch. The response exposes lower latest_block, so the defect is not undetectable, but the recipe currently fails to check it.

**Disposition / plan change:** Reject continuation when current serving watermark is below cursor W; return backendUnavailable, not outsideRetention. Test ascending and descending walks. SDK must not checkpoint W after any response whose serving bound is below W. Do not require durable cross-reset cursors.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1989-L2030)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2387-L2406)
- [Source 3](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1106-L1142)

#### ADM-2: accept with change

**Reviewed claim:** Exact-byte resubmission, the core SDK recovery path, hits open hogd mempool bugs on current main

Main still recognizes only queued transactions as pending. PR4468 fixes nominated-cache dedupe; PR4471 adds the missing full-capacity regression coverage and is not a distinct implementation fix. Neither is merged. Retained Succeeded outcomes cannot be overwritten; the reported overwrite danger is in nomination or after retained outcome eviction.

**Disposition / plan change:** Require #4468-equivalent nominated-duplicate correctness and #4471-equivalent full-capacity coverage before announcing public submission, even if automatic SDK retry remains disabled. Raw callers can manually duplicate a signed envelope. Main can requeue a nominated transaction, append its hash twice and fail commit cleanup when the cache entry is removed twice. This is a runtime safety gate as well as a recovery dependency. Qualify nomination chunks/full queue/abandonment/reenqueue/postcommit before enabling automatic exact-byte recovery; until then only proven never-dispatched attempts with no earlier uncertainty may auto-retry.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L667-L700)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L540-L575)
- [Source 3](https://github.com/thruputchain/hogd/pull/4468)
- [Source 4](https://github.com/thruputchain/hogd/pull/4471)
- [Source 5](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L1454)

#### HIST-2: accept

**Reviewed claim:** The history integrity minimum misses other silent-exhaustion paths, including on root-contract history

Both event and root-transaction ContractIter treat invalid RocksDB iteration and malformed in-range keys as exhaustion. Hydration skips all failures; PR4454 only fixes nonmissing errors. Receipts and single simulation also silently drop malformed projected events.

**Disposition / plan change:** Expand GA minimum to iterator I/O/corruption status, malformed in-range keys, all missing/unreadable/undecodable linked rows, sequence overrun, and invalid projections. Fail the whole response with typed error and no successful page/cursor. Explicitly cover root history, receipts and single/batch simulation. Missing linked rows are inconsistent under current no-pruning atomic index model, not skippable.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1843-L1923)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2116-L2150)
- [Source 3](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2433-L2464)
- [Source 4](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/receipt.rs#L17-L48)
- [Source 5](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-gateway/src/handlers/view.rs#L134-L140)
- [Source 6](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/view.rs#L80-L85)
- [Source 7](https://github.com/facebook/rocksdb/wiki/Iterator)
- [Source 8](https://github.com/thruputchain/hogd/pull/4454)

#### ADM-1: accept with change

**Reviewed claim:** Submit error mapping does not match the scheduler's four admission outcomes; unfunded accounts become 'backend unavailable' with a retry loop

The four submit outcomes are request-scoped. Both Retryable and InternalError prove this attempt was not admitted. Retryable is used for stale auth, not-ready and capacity, so overload alone is too narrow. InternalError also includes ambiguous missing/unfunded account versus failed lookup.

**Disposition / plan change:** Add explicit outcome mapping: Admitted success; AuthenticationRejected -32001 admission=rejected; Retryable -32053 admission=rejected with truthful temporary-admission-unavailable semantics, not capacity assertion; InternalError -32050 admission=rejected. Transport/cardinality/unknown-result failures after possible dispatch remain unknown. Do not use any current-attempt rejection as proof an earlier attempt did not commit. Document funded scheduling-account precondition; do not invent insufficientFunds.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/sched_gateway.x#L20-L39)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/rpc_from_coord.cc#L177-L185)
- [Source 3](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/rpc_from_coord.cc#L428-L433)
- [Source 4](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/rpc_from_coord.cc#L541-L555)
- [Source 5](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L733-L753)
- [Source 6](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/scheduler.rs#L24-L43)

**Follow-up:** Separate unknown/unfunded scheduling account from dependency failure in DB/scheduler protocol; until then onboarding must describe ambiguous diagnosis and bounded retries.

#### HIST-3: accept

**Reviewed claim:** 'Preserve fail-fast consistency checks' keeps query-triggered panics in the indexer read path

Public event hydration uses unconditional CHECK_EQ; a corrupt row can crash indexer on query rather than produce the promised data-invalid error.

**Disposition / plan change:** Preserve read consistency checks as fail-closed typed query errors. Keep ingestion invariants where appropriate. Qualify poisoned block/emitter rows without process termination or partial page.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2132-L2150)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/base/log.h#L372-L384)

#### HIST-4: reject

**Reviewed claim:** oldestIndexedBlockHeight of 0 is not always true; the backend already holds an authoritative lower bound

Strict fresh index initialization explicitly requires epoch_sequence=0 and first_height=1 with genesis linkage; subsequent epochs must continue the durable boundary. The reviewer confuses later event-server epoch origins with a fresh indexer origin. An arbitrary first_height cannot enter a valid fresh strict index. oldest=0 includes genesis and does not conceal the alleged gap.

**Disposition / plan change:** Keep zero as current genesis-backed lower bound; qualify the new devnet uses strict genesis-anchored indexing. Retention and legacy migration remain separate explicit follow-ups, not a new non-genesis-start GA repair.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1106-L1142)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1710-L1757)
- [Source 3](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.h#L143-L160)

#### EVT-1: accept with change

**Reviewed claim:** Event topic0 also depends on the interface crate's version, which the native token hard-codes as 0.1

Signature uses the package-derived major component, so 0.1.x->0.2.x or1.x can change it despite wire_crate pin. But the claimed absence of a cross-repo test is false: native_rv32_token_event_parity_test already compares generated RV32 events to native bytes; built_contracts pins0.1.58.

**Disposition / plan change:** For this release explicitly freeze the token interface wire-signature version at0.1 and wire_crate groundhog-erc20-if, including error selectors; no package-major/minor bump across that identity boundary. Run existing parity tests against exact candidate pins and preserve expected topic0/selector vectors in fixtures. An explicit event wire-version override is a useful follow-up if the package must move to0.2/1; do not require new macro machinery solely for this cut.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-sdk-rs/blob/12337492533acfa45a475cabc5af88ffe456559d/crates/contract-util/src/lib.rs#L264-L283)
- [Source 2](https://github.com/thruputchain/thruput-sdk-rs/blob/12337492533acfa45a475cabc5af88ffe456559d/crates/macros/src/derive_event.rs#L298-L313)
- [Source 3](https://github.com/thruputchain/thruput-sdk-rs/blob/12337492533acfa45a475cabc5af88ffe456559d/standard-interfaces/thruput-erc20/src/error.rs#L4-L5)
- [Source 4](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/precompiles/precompile_native_token.cc#L869-L896)
- [Source 5](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/contract_tests/native_rv32_token_event_parity_test.cc#L371-L419)
- [Source 6](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/built_contracts/Cargo.toml#L75)

**Follow-up:** Explicit independent wire-version pin for event derive before a future interface package crosses0.1.

#### RT-1: accept

**Reviewed claim:** Current hogd main removes --nthreads, which packaging and ops launch configs still pass, so hog-exec exits

Current hog-exec rejects unknown flags; nthreads is absent from parsing. Current packaging and ops still pass it. This is a concrete candidate startup failure, not merely a hypothetical smoke concern.

**Disposition / plan change:** Remove obsolete --nthreads/-n and unused EXEC_NTHREADS wiring in launcher consumers before candidate assembly; test actual packaged validator/local launch commands. Place preparation in runtime/consumer track, landing before artifact build and slice8 qualification.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/hog_exec.cc#L126-L131)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/hog_exec.cc#L159-L198)
- [Source 3](https://github.com/thruputchain/packaging/blob/73566ec94a6152b5bc4899b15e43ca005aa60b3c/config/hogd/exec.env#L1)
- [Source 4](https://github.com/thruputchain/packaging/blob/73566ec94a6152b5bc4899b15e43ca005aa60b3c/docker/entrypoint-hog-core.sh#L114)
- [Source 5](https://github.com/thruputchain/packaging/blob/73566ec94a6152b5bc4899b15e43ca005aa60b3c/docker/entrypoint-validator.sh#L87)
- [Source 6](https://github.com/thruputchain/ops/blob/f03432c92a4f3af7375306a24bc7f680207c451b/ops-plan/src/run/validator.rs#L1077)
- [Source 7](https://github.com/thruputchain/ops/blob/f03432c92a4f3af7375306a24bc7f680207c451b/scripts/hosted-bootstrap-local-smoke.sh#L341)
- [Source 8](https://github.com/thruputchain/ops/blob/f03432c92a4f3af7375306a24bc7f680207c451b/tofu/modules/devnet-nightly/files/configure-validator-suite.sh.tftpl#L33)

#### STAT-1: accept with change

**Reviewed claim:** Status precedence returns a scheduler non-commit outcome when the indexer read failed

Scheduler Some currently suppresses indexed errors; retained Succeeded normally prevents false downgrade, but retention/eviction and later attempts permit non-commit diagnostics without durable lookup. Existing precedence conflicts with a general backend-errors-remain-errors promise.

**Disposition / plan change:** Indexed committed always wins; scheduler committed can answer despite indexer failure; scheduler pending is provisional presence, not absence of prior commitment. On indexed error plus scheduler retry/dropped/recent diagnostic or miss, propagate backend error. Never erase client-known commitment. Add exact fixtures.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/status.rs#L39-L58)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L540-L575)
- [Source 3](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L449-L458)

#### PROTO-1: accept with change

**Reviewed claim:** History work splits into hogd-only parts and protocol-carrier parts; stage them

Cursor storage is hogd-internal; current query outcome carries only not-ready/internal failure. Safe failure can land first without protocol changes, but final promised invalidCursor/historyUnavailable/data-invalid mapping requires typed carriers and consumers. Plain cursor encoding has no authentication.

**Disposition / plan change:** Stage backend fail-closed repairs before typed protocol/bindings/gateway release. Final GA still requires promised typed errors. Query-binding digest and version bump must fit256-byte cap and distinguish method+query identity; call it accidental-mismatch detection, not tamper-proof authorization. Include4458 direct seek with the follow-up candidate cap in slice2; general time/byte budgets and cancellation remain deferred.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/event_cursor.h#L15-L31)
- [Source 2](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/xdr/event_cursor_internal.x#L1)
- [Source 3](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/indexer.x#L229-L259)
- [Source 4](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/indexer.rs#L42-L51)
- [Source 5](https://github.com/thruputchain/hogd/pull/4458)

#### DOC-1: accept with change

**Reviewed claim:** The receipt deterministicError kind is a gateway constant, not backend-provided

Backend ExecResult contains no deterministic-error subtype. The gateway supplies constant fault.

**Disposition / plan change:** The current adapter manufactures deterministicError.kind as fault. Remove that redundant field in the unified receipt fault variant (ETH-2/TT-6), rather than documenting it as a producer diagnostic. No out-of-gas diagnosis or finer fault reason without typed evidence.

**Reverified evidence:**
- [Source 1](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/exec_result.x#L3-L41)
- [Source 2](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/receipt.rs#L52-L67)

### ts review

#### CR-1: accept

**Reviewed claim:** thruput-react is a missed SDK consumer and breaks on base58 because it lowercases hashes and addresses

React is a real missing published consumer despite zero literal RPC method references. Lowercasing case-sensitive base58 corrupts identifiers; ^0.18.0 excludes 0.19.0.

**Disposition / plan change:** Add React to slices 4/7 and GH-506 coordination; typed case-preserving codecs, network/query keys, matching peer range and release, then update all example pins. A method-name grep is not a complete consumer inventory.

**Reverified evidence:**
- react@5e32308 src/transactions.ts:59-70; balances.ts:175; query-key.ts:27; wallet-store.ts:101; package.json:46
- examples@c8ba873 apps/{counter,swap,token-launchpad}/package.json

**Scope:** GA

#### CR-2: accept with change

**Reviewed claim:** Application proxies are partial endpoints, but the discovery, fallback and typed-error contract assumes the server is the endpoint

The plan already calls for proxy migration; the missing piece is an explicit contract for a filtered endpoint, not proof that the intended final implementation necessarily fails.

**Disposition / plan change:** Treat an app proxy as an endpoint: allow getVersionInfo/getNetworkInfo, report intersection of upstream supportedMethods and proxy policy, preserve bounded typed error data, return JSON-RPC -32601 for filtered known methods, support named params, and update numeric-code branches. Authenticate/rate-limit at HTTP boundary separately. Test actual proxied SDK flows; do not require SDK callers to invent capability declarations.

**Reverified evidence:**
- examples@c8ba873 apps/swap/server/http.ts:53-60,417-429,515-585
- explorer@ec1b465 src/lib/groundhog/rpc.ts:34

**Scope:** GA

#### CR-3: accept with change

**Reviewed claim:** The single-call funding default disables the hosted faucet's own request-driven recovery and contention handling

A single call gives up automatic recovery from clean pre-attempt contention; retryAfter alone does not prove globally safe replay once an earlier request was ambiguous. The follow-up review confirms that the current wire contract lacks positive pre-attempt proof, so this major deliberately accepts that convenience tradeoff. Pending recovery is request-driven. A later current-store miss cannot disprove a previous attempt lost on restart.

**Disposition / plan change:** Updated after the follow-up PR review: disable automatic hosted funding-service repeats for this major, including policy refusals and pending polls. Do not add a new evidence field solely for that optimization. Keep nonce/network/recipient/hash evidence on interruption. After possible execution, default to read-only reconciliation; return explicit same-nonce resume evidence if reads cannot finish. Do not silently regenerate nonce or promise durable hosted idempotence. A resume that calls the nondurable service requires explicit caller opt-in to its documented duplicate-test-fund risk. Durable recovery stays follow-up. Preserve existing producer retry hints through checked seconds-to-milliseconds conversion in the narrow GA adapter scope; invalid hints fail closed instead of falling back to an earlier delay. Delay hints serve explicit caller retries; they do not authorize automatic service calls.

**Reverified evidence:**
- tools@d0a8cdc9 crates/faucet/src/airdrop.rs:144-179,357-378,387-425,627-648,727-747
- sdk-ts@8ce62f4 src/actions/public/requestAirdrop.ts:66-124

**Scope:** GA

#### CR-4: accept with change

**Reviewed claim:** Exact-byte retention and resubmission need public API changes in the local and provider wallet flows; the plan says existing composition remains

Existing composition is compatible with extending evidence carriers; a blanket workflow rewrite is unnecessary. But exact bytes/network/hash are not presently retained on all error/no-wait paths, and provider approval is consumed once.

**Disposition / plan change:** Name immutable SubmissionEvidence {signedTransaction, transactionHash, networkHash}; carry it on submission/observation errors (including cancellation after signing), with strongest known commitment and original causes. Add local onSigned persistence hook matching provider capability before first dispatch. Public low-level sendTransaction permits explicit same-byte resubmit only after verified same network/replay preconditions; do not replay the consumed provider submit approval or silently bypass it automatically. Provider/session revocation must stop automatic actions.

**Reverified evidence:**
- sdk-ts@8ce62f4 src/wallet/execution.ts:39-79; src/wallet/client.ts:274-339
- sdk-ts@8ce62f4 src/wallet/providerClient.ts:430-464; src/actions/wallet/sendInvocation.ts:37

**Scope:** GA

#### CR-5: accept with change

**Reviewed claim:** The 'invalidate discovery on reconnect or endpoint change' acceptance is not observable through the SDK Transport abstraction

Current HTTP/custom transport exposes no reconnect event. New instance on changed endpoint is already sufficient in common use.

**Disposition / plan change:** Scope discovery cache to client/transport instance; new instance means new validation. Expose explicit invalidation for mutable custom transports; invalidate/revalidate after incompatible known-method response, -32601 or network mismatch. Do not transparently replay mutations after refreshing. Remove imaginary HTTP reconnect criterion and prohibit mixed incompatible backends behind one endpoint.

**Reverified evidence:**
- sdk-ts@8ce62f4 src/transport/types.ts:8-29; src/transport/http.ts:162-190
- examples@c8ba873 apps/counter/src/thruput/public-client.ts:21-30

**Scope:** GA

#### CR-6: accept with change

**Reviewed claim:** Freighter's single-major gate plus a native-only SDK leaves no Freighter build that works both before and after the switch; PR #3 targets a wire 12 that the public endpoint may never serve

Gate mismatch is real, but there are no existing external users or cutover/update window to maintain.

**Disposition / plan change:** Retarget the final native wallet build to the chosen major and provider version, update network pins for the fresh deployment, and qualify installation/connect/sign/reopen before announcement. Wire12 PR can be an internal development baseline only. No two-major wallet adapter or distribution sunset window required.

**Reverified evidence:**
- freighter@577cd7c0 extension/src/background/thruput/network.ts:22-48
- freighter#3 head1163d800 extension/src/background/thruput/network.ts:17-25,51-57 (current open head re-read)

**Scope:** GA

#### CR-7: accept with change

**Reviewed claim:** 'Canonical formats in saved data' with no legacy hex fallback conflicts with preserving persisted pending-transaction evidence

Old pending envelopes cannot become current PQ transactions by text conversion. Fresh-devnet context explicitly permits discarding old chain state.

**Disposition / plan change:** Version/reset Thruput-specific cached accounts, approvals, pending records and history for fresh devnet; do not auto-resubmit old signed evidence. Preserve Stellar vault and key material. Canonical new stores use base58/hex rules; no legacy-reader migration gate. If networkHash can remain same across reset, version namespace or explicit reset marker must prevent stale evidence reuse. Future durable cross-reset recovery remains deferred.

**Reverified evidence:**
- examples@c8ba873 apps/swap/src/transactions/crank-attempt.ts:5-6,35-52
- freighter@577cd7c0 extension/src/thruput/networks.ts:6-16,39-40
- Linear GH-511 current description: version saved Thruput data, start fresh; preserve Stellar vault/key derivation

**Scope:** GA

#### CR-8: accept with change

**Reviewed claim:** The new helper names duplicate existing exported, documented helpers with different unknown-variant semantics

Existing exported helpers are real; reviewer overstates docs references to assertTransactionSuccess (docs name waitForExecution and error classes).

**Disposition / plan change:** Keep assertTransactionSuccess and waitForExecution, evolve them for nested execution and typed unsupported outcomes; remove proposed parallel assertSuccessfulExecution name. Update generated bindings/docs/agent guidance consistently. No need redundant aliases in native-only breaking SDK.

**Reverified evidence:**
- sdk-ts@8ce62f4 src/wallet/execution.ts:17-30,82-90,105-111
- docs@8b32849 src/content/docs/tools/typescript-sdk.md:79-83; typescript/transaction-status.md:67-85
- agent-skills@a976723 skills/thruput/references/typescript.md:90,152-155

**Scope:** GA naming/docs

#### CR-9: accept

**Reviewed claim:** knownCommitment has no network context although the plan requires network validation

A bare hash/height cannot enforce the stated network check.

**Disposition / plan change:** KnownCommitment is {networkHash, transactionHash, blockHeight}; waitForCommitment returns it, receipt wait verifies matching hash and network before using it, and interruption errors preserve it. Network hash does not identify a reset, so same-network reset evidence remains explicitly invalidated by application/deployment generation.

**Reverified evidence:**
- sdk-ts@8ce62f4 src/actions/public/waitForTransactionReceipt.ts:23-27,156-171
- companion selected knownCommitment type omits networkHash

**Scope:** GA

#### CR-10: accept with change

**Reviewed claim:** The SDK's static cluster capability model is not reconciled with runtime supportedMethods

Static capabilities currently control type/action attachment; runtime discovery is a separate source. A full type-system redesign need not gate GA.

**Disposition / plan change:** Declare static cluster capabilities expectations, not health/discovery truth. Runtime supportedMethods is authoritative; fail a requested unavailable optional action locally with typed unsupported-method evidence. Keep discovery fresh per client policy. Defer dynamic TypeScript capability redesign, with an explicit SDK follow-up.

**Reverified evidence:**
- sdk-ts@8ce62f4 src/cluster.ts:15-33,96-118; src/client.ts:284-316

**Scope:** GA clarification; type redesign follow-up

#### CR-11: reject

**Reviewed claim:** Minor count and head drift

98 is a substring-count artifact, not another method occurrence. Original97 is correct. Source head drift is real but has no public-method count effect.

**Disposition / plan change:** Keep count97 with exact matching definition and origin revision. Add React as zero-literal consumer. Counts remain search aids, not completeness evidence.

**Reverified evidence:**
- Current ops@f03432c exact word-boundary count for 19 declared RPC names =97; same source includes groundhog_chainTipExtra in scripts/tests/test_ebpf_gateway_apply.py:85
- origin head current f03432c92a4f3af7375306a24bc7f680207c451b

**Scope:** doc-only

### plan review

#### DEL-1: accept with change

**Reviewed claim:** The 'current tuple' kept through the preparation window already breaks new users: the latest CLI and SDK speak wire 12 but default to an 11.2 endpoint

Source/release/hosted mismatch is confirmed; user impact remains inferred, no mutation/failure test run. No real users need compatibility preservation.

**Disposition / plan change:** Record current internal mismatch only as baseline. Final build/package pins, defaults, manifests, docs, wallet and deployed fresh endpoint must agree before announcement. Do not require interim hosted12 or old-version docs solely to maintain a nonexistent preparation window.

**Reverified evidence:**
- tools@d0a8cdc9 API lib.rs:35 wire12; toolchain catalog:4 rc7; CLI config/global.rs:14 default hosted URL
- sdk-ts@8ce62f4 cluster.ts:10 and package.json0.19.0
- Current read-only groundhog_version:11.2.0/tools0.29.2/rc6/a2f492ea; current GitHub latest tools release v0.30.0 2026-10-14T22:55Z

**Scope:** announcement readiness

#### DEL-2: reject

**Reviewed claim:** Ship wire 12 (runtime/PQ reset) to hosted separately from wire 13 (API/encoding); this split is what makes a cheap bridge and a real rollback possible

The staged-hosted transition is optional for mature users, not an engineering requirement; here it adds deployments/qualification and relies on an unchanged whole-protocol token despite in-flight protocol work.

**Disposition / plan change:** Use land slices -> one final artifact set -> fresh devnet -> qualify -> announce. No hosted12 precursor, legacy gateway, dual-wire probes or binary-rollback boundary. Binding/runtime agreement still mandatory within final set.

**Reverified evidence:**
- tools@d0a8cdc9 gateway context.rs:243-252 handshake; current freighter#3 body account derivation change
- Stellar v22 compatibility behavior confirmed https://github.com/stellar/stellar-rpc/releases/tag/v22.0.0
- Confirmed context: internal-only devnet, no state to preserve; deployment requires separate approval

**Scope:** transition removed

#### DEL-3: accept with change

**Reviewed claim:** No branch or release-freeze policy reconciles 'no long-lived branch' with 'never expose half-migrated contracts', and GH-507 independently bumps the major from unpushed local code

Merge to main does not auto-publish tools or SDK. Release freeze is sufficient; no reason to demand long-lived integration branch. Local commits are not the same as uncommitted or independently inspected work.

**Disposition / plan change:** Select short manual release freeze for affected tools/SDK main once partial contract changes land. One coordinated wire-major owner and fixtures; GH-506/507 do not independently publish a competing contract. Inspect exact shared diff (reviewable PR or supplied local patch allowed) and agree file ownership before overlapping edits; publishing/pushing requires separate authorization. Permit internal exact-SHA candidate builds, with any required bootstrap prerelease explicitly controlled. Freeze lifts only for reviewed coherent artifact chain. No code or workflow changes now.

**Reverified evidence:**
- tools@d0a8cdc9 .github/workflows/publish.yml:3-4,61-73,79-139
- sdk-ts@8ce62f4 .github/workflows/publish.yml:3-4,17,42-47,70-101
- Current GH-507 description and comments: independent major bump listed, local reported commits exist but no pushed reviewable PR

**Scope:** start/landing/release rule

#### DEL-4: accept with change

**Reviewed claim:** Omitting history is not a usable fallback; shrink the history minimum to hogd-only fail-closed behaviour so it leaves the protocol critical path

History omission breaks the selected full product. Reject proposed final InternalFailure-only/query-binding deferral: malformed input would look like transient backend fault and it contradicts GH428. GH513 binary-layout constraint is scoped to encoding, not a ban on other protocol work.

**Disposition / plan change:** Keep expanded typed history minimum including invalid cursor/query binding/continuation integrity in final GA. Narrow fail-closed internal patches can land early but do not satisfy final contract. Any history omission requires explicit reduced product scope and disabling/reworking named consumers, not treating full GA as passed.

**Reverified evidence:**
- protocol@af043b5 indexer.x:229-237; tools@d0a8cdc9 crates/x23/hogd-backend/src/indexer.rs:42-56
- tools CLI tx.rs:164/events.rs:183; explorer@ec1b465 addresses.ts:79,101,106; ops@f03432c smoke-devnet-rv32.sh:295,304
- Current GH-428 explicitly requires canonical protocol invalid-cursor result ->-32602

**Scope:** GA protocol dependency

#### DEL-5: accept with change

**Reviewed claim:** In-flight hogd indexer PRs overlap slice 2 and #4454 takes the opposite decision on missing hydration rows

#4454 narrows silent skipping in the same direction; it is not an opposite design. Missing hydration is not currently proven legitimate. #4458 is targeted seek-work repair, not full scan budgeting.

**Disposition / plan change:** Add both PRs to slice2 coordination. Retain fail-closed missing/corrupt/out-of-range/inconsistent rows, propagate iterator errors and reject invalid seeks. Establish source-backed exception before relaxing. No silent skip plus metric as final integrity contract. Coordinate actual diff/merge heads with indexer owner before edits.

**Reverified evidence:**
- Current hogd#4454 head38d4a5d54b6771b32ddc64761c20ccbe561c10fd; #4458 head83a3fc42af2420d7cd363e3901a12bff6dd4740f
- runtime verification against current hogd: hydration and contract rows atomically ingested; no supported retained pruning explains missing linked row

**Scope:** history sequencing

#### DEL-6: accept with change

**Reviewed claim:** The artifact publication order has a cycle (tools templates ↔ SDK ↔ devnode catalog) that the slices do not resolve

This is a staged artifact dependency, not unsolvable cycle. Existing local integration overrides do not themselves override the hosted SDK publication workflow.

**Disposition / plan change:** Build/qualify protocol+bindings/runtime first; publish exact public devnode needed by catalog; build and when required publish an explicitly controlled tools bootstrap RC through the normal workflow as SDK generator when needed; qualify/publish SDK; pin final tools templates to it; build final tools and packaging bundle/images; publish matching React/apps/wallet. Deploy only final set. Candidate/local artifact qualification may use THRUPUT_CLI/THRUPUT_DEVNODE_BINARY. Do not claim npm prerelease isolation exists: current publish has no --tag next, so add/verify candidate-channel support or keep candidate packages local until coordinated controlled publication.

**Reverified evidence:**
- tools@d0a8cdc9 counter-app/package.json.tmpl:14 and publish.yml:61-73
- sdk-ts@8ce62f4 scripts/run-integration.mjs:179-261 overrides; .github/workflows/publish.yml:17,42-47 published CLI requirement
- packaging@73566ec scripts/qualify-release.sh:208; .github/workflows/publish-devnode.yml:4-13

**Scope:** artifact order

#### DEL-7: accept with change

**Reviewed claim:** The GA scope couples wire-independent behaviour work to the break, and expands GH-411 into the launch gate without saying so

Wire-independent work can land independently, but being additive does not justify unsafe default resubmission or violating a selected GA contract. The proposed release scope includes native API qualification before announcement.

**Disposition / plan change:** Separate optional new helper convenience from required migrated safety behavior. Existing flows must preserve known hash/network/commitment, validate response identity, retain unsupported outcomes, and avoid blind ambiguous mutation retry. Do not defer whole admission or history semantics under a wire-only label. Record native API as proposed announcement dependency in Linear later, leaving broad GH411 unrelated ergonomics outside GA.

**Reverified evidence:**
- Current GH-411 explicitly not launch dependency by default; GH-470 initially names wire rename
- sdk-ts@8ce62f4 requestAirdrop.ts:66-124 current retry behavior; execution.ts current helpers

**Scope:** scope refinement

#### DEL-8: accept with change

**Reviewed claim:** The qualification gate depends on unowned load and resource work (GH-432, GH-422, GH-500 resource bounds)

Unowned measurement is a real readiness gap, not evidence the API is unsafe or a reason for a separate hosted12 deployment.

**Disposition / plan change:** Before announcement assign qualification owner, fixed dataset construction/size, selective/no-match/dense query mix, PQ/Ed/mixed submissions, concurrency and current limits, run duration and numerical acceptable latency/resource/recovery thresholds. Include after-timeout/abandonment work. Record actual results on final fresh-devnet artifacts. Keep threshold/owner selection outstanding; do not invent them or silently waive GH432 public-launch admission policy.

**Reverified evidence:**
- Current GH-432/GH-422 Todo unassigned; current GH-500 resource qualification incomplete
- tools gateway existing timeouts cannot establish downstream cancellation (runtime agent source verification)

**Scope:** announcement gate

#### DEL-9: accept with change

**Reviewed claim:** Ops and packaging probes need dual-wire reads through the window and rollback; a temporary version-only legacy method is cheap

Probe/metrics migration is real; dual-wire fallback and legacy version alias exist only to support the discarded transition.

**Disposition / plan change:** Migrate probes to final methods/named params/fields/canonical codecs; require expected final wire and artifact identity. Regenerate eBPF method labels/map sizing and gateway symbols together with build; coordinate #230. No dual-wire probes or temporary groundhog_version alias.

**Reverified evidence:**
- packaging@73566ec scripts/qualify-release.sh:545-560; scripts/smoke-hosted-gateway.sh:48
- ops@f03432c scripts/check-validator-runtime-config.py:1466,1578-1596; smoke-devnet-rv32.sh:201; monitoring/collect-faucet-balance.py:50-66
- Current ops#230 head4ae3a728ce47382751727d835f8b89072789fead

**Scope:** GA operational consumer migration

#### DEL-10: accept with change

**Reviewed claim:** Owner concentration: the in-flight encoding issues the plan depends on and the wallet boundary all sit with one assignee

File overlap and review/qualification concentration are real. Existing role descriptions were not assignments.

**Disposition / plan change:** Keep named owners and exact shared-file boundaries explicit. Inspect actual GH-506/507 diff and resolve ownership before overlapping implementation starts after explicit implementation hold release. Independent work may be authorized separately. Native API qualification gets a separate proposed evidence home linked to GH-512; no implicit reassignment.

**Reverified evidence:**
- Current GH-507/GH-506/GH-512/GH-511 assigned chambaz; GH-413/GH-411/GH-422/GH-432 unassigned, re-read15Oct
- Current GH-507/506 comments describe local unpublished implementation

**Scope:** execution readiness

## Implementation gaps and proposed tracker changes

No code or runtime result is implied by these dispositions. Required work and open owners/thresholds are in the [main readiness criteria](../delivery-plan.md#review-and-readiness). The proposed Linear edits there are not applied. Deferred ETH-4 and STL-2 have explicit qualification boundaries; accepted-with-change items also identify deferred portions instead of hiding them in GA scope.

Implementation remains on hold pending explicit approval.


## Follow-up PR review

The second review examined draft PR #555 at `b98d4b8`. Its sections below use
that review's labels, separate from the original 65-ID count. Source verification
on 15 October rechecked tools `d0a8cd…`, hogd `15d536…`, protocol `af043b…`,
examples `c8ba87…`, SDK TS `8ce62f…`, packaging `73566e…` and ops `f03432…`
against origin. chuckd was additionally checked at
`6db13472df621feba39626c49e65e292f8baef16`. No runtime tests were run. These are
accepted proposal revisions, not evidence of implementation or qualification.

| Review item | Disposition | Selected change or correction |
| --- | --- | --- |
| 1A event scan/cursor | Accept with change | Slice 2 includes a positive examined-candidate cap, last-validated-visited cursor and direct seeking. Test ascending/descending, overlapping filters, no-match pages and exhaustion. Current empty-nonterminal branch is unreachable without an earlier scan stop; the example is a target shape. Reject the estimate that a tiny patch removes resource qualification. |
| 1B chuckd | Accept with change | Add hosted bootstrap metadata → chuckd → runtime manifest → tools, matching Go bindings and final packaging pin. Only networkHash enforces lowercase unprefixed hex today; initialization hashes also accept prefixed/uppercase hex. Raw XDR stays hex. Nested commit.valueHash is not currently parsed by tools, so that field alone is not its startup blocker. |
| 1C.1 structural params | Accept with change | Keep strict DTOs; generic serde failures may supply only invalidParams. Optional typed reasons/path require reliable evidence. No message parsing, second general validator or unknownField-based discovery dependency. Path tracking can retain some paths; not all tagged-enum inner paths are guaranteed. |
| 1C.2 funding retry proof | Accept with change | Disable automatic hosted service repeats this major, including refusal/pending. Preserve explicit resume and hints. Do not add admission or a fundingDispatch field for the optional optimization. Stronger service evidence and automatic recovery follow with durability work. |
| 1C.3 submission timeout | Accept with change | Preserve evidence across queue expiry, cancellation, sink handoff and outer deadlines in gateway/local paths. Claim notAttempted only when later dispatch is impossible. A precheck timeout before hashing can omit the hash. |
| 1C.4 error schema | Accept with change | One named component per numeric code, with typed data unions covering all 12 kinds across 11 codes and valid operation-specific evidence. Existing components already use names; numeric lookup is the limitation. Per-method error codes must be unique. |
| 1D client/schema | Accept with change | Replace macro-generated wrapped clients with thin flat-DTO ClientT adapters; choose explicit versus small generated wrappers in the spike. Add paramStructure by-name and whole-object extension. No mandatory broad macro rewrite. |
| 1D continuation | Accept | Supply valid ignored XDR order/anchor placeholders; backend continuation uses cursor-owned state. No XDR layout break solely for omitted start. |
| 1D registration | Reject claimed API limitation | Pinned jsonrpsee has remove_method. Retain final registration/discovery agreement before serving; register-only builders or an initialized immutable method list are implementation choices. |
| 1D server limits | Accept | Omit simulationBatch when its method is unconfigured. Temporary health does not alter support metadata. |
| 1E Rust waiter/receipt hash | Accept with change | Retain strongest commitment in shared Rust wait/error paths; validate backend log hash and height before receipt/error observation. Poll ordering is an optimization, not a required rewrite. |
| Simplification 1 buffers | Accept with change | Require returnData/abortData for current respective receipt variants. No old-chain missing-buffer case. Retain ABI decode errors and supported ExecResult/root-output combination checks; failed commit can follow root Return. |
| Simplification 2 observation | Accept | Receipt-error observation contains only validated transactionHash and blockHeight. No partial execution/gas, and no observation fabricated from a mismatched log. |
| Simplification 3 detail probe | Accept with change | Remove unsupported call/deploy probe and diagnostics. It runs beyond verbose mode. Preserve view's event formatter; audit public DTO/test/dependency references before deleting them. |
| Simplification 4 tolerant readers | Accept with change | Rust and TS preserve unknown tag/payload. Reject the claim that serde(other) rejects extra fields: it accepts then discards them. Unknown XDR adapter values need their own supported projection or typed backend-data failure. |
| Simplification 5 metrics | Accept | Cover final method set and error catalogue with bounded labels, including framework size errors and an unknown bucket. Do not label arbitrary incoming strings. |
| Simplification 6 proxies | Accept with change | Cover Counter, swap and token-launchpad separately. Only swap currently strips typed error data; all require native migration. Shared helpers are optional; preserve distinct write policies. |
| Simplification 7 branch pushes | Reject mandatory publication | Require exact inspectable GH-506/507 diffs and ownership, not branch pushes. A patch or local review suffices; publication needs separate owner authorization. |

### Source evidence for these revisions

- Event loop/filter/cursor: [hogd indexer](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L2090-L2214), [current skip-past](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_indexer/db.cc#L1861-L1885), and [direct seek #4458](https://github.com/thruputchain/hogd/pull/4458). Candidate counting does not bound initial seeks, repeated full-log decoding, RocksDB read latency or cancellation tails.
- Receipt identity and lifecycle: [shared adapter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/x23/hogd-backend/src/receipt.rs#L29-L48), [Rust waiter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-client/src/receipt.rs#L155-L248), [root-output union](https://github.com/thruputchain/thruput-protocol/blob/af043b5c1983c0c8b44683975a3c389369844e09/transaction.x#L88-L102), [commit path](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/runtime/syscall_handler.cc#L537-L662), and [buffer producer](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_exec/runtime/syscall_handler.cc#L1319-L1333).
- Hosted artifact path: [chuckd bootstrap](https://github.com/thruputchain/chuckd/blob/6db13472df621feba39626c49e65e292f8baef16/chuckd/cmd/hosted_bootstrap.go), [Go pin](https://github.com/thruputchain/chuckd/blob/6db13472df621feba39626c49e65e292f8baef16/go.mod), [tools runtime manifest](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/runtime_manifest.rs), and [packaging pin](https://github.com/thruputchain/packaging/blob/73566ec94a6152b5bc4899b15e43ca005aa60b3c/release-manifest.toml#L21-L26). Current Go pin is `v0.0.0-20261012233407-471321271e0f`; packaged chuckd is `v0.1.31-rc.6` at `4cb36e49`, not current main. Qualify final installed pins through the existing hosted-bootstrap smoke.
- Dispatch/error boundaries: [submitter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-gateway/src/submitter.rs), [shared deadlines/limits](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-server/src/server.rs), [faucet adapter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-gateway/src/airdrop.rs), and [error schema generator](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/open-rpc-spec/src/lib.rs).
- CLI cleanup: [detail probe/formatter](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/cli/src/dev_detail.rs), [call](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/cli/src/call.rs#L148), [deploy](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/cli/src/deploy.rs#L262), and [view formatter consumer](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/cli/src/view.rs#L19).
- App proxies: [Counter](https://github.com/thruputchain/thruput-examples/blob/c8ba8730d0d06e027dc939afb5974e32992f264f/apps/counter/server/http.ts), [swap](https://github.com/thruputchain/thruput-examples/blob/c8ba8730d0d06e027dc939afb5974e32992f264f/apps/swap/server/http.ts), and [token-launchpad](https://github.com/thruputchain/thruput-examples/blob/c8ba8730d0d06e027dc939afb5974e32992f264f/apps/token-launchpad/server/http.ts).

Pinned dependency checks used installed source: jsonrpsee 0.25.1
`core/src/server/rpc_module.rs:599` exposes `remove_method`;
`proc-macros/src/render_client.rs:230–271` wraps arguments. Serde 1.0.229
`private/de.rs:2990–2996` drains extra fields through `InternallyTaggedUnitVisitor`;
`serde_derive/src/de/enum_internally.rs:82–93` selects it. serde_json 1.0.151
`error.rs:433–450` retains structural failures as text. These mechanics were
verified in the exact local dependency sources; failed upstream source fetches
are not cited as independent verification.

External conventions were checked against [Serde variant attributes](https://serde.rs/variant-attrs.html)
and the [OpenRPC specification](https://spec.open-rpc.org/). The latter's current
1.4.x spec defines `paramStructure` and unique per-method error codes. Validation
against the implementation's selected meta-schema, including any retained 1.2.6
version, remains a slice-1 check; no successful archived-schema fetch is claimed.

The duplicate-submission announcement gate remains unconditional: [cached
transaction requeue](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L667-L685),
[duplicate nomination](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L1290-L1305)
and [commit cleanup](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L1451-L1454)
support the #4468-equivalent repair/#4471-equivalent coverage requirement even
when SDK recovery is off. Broader automatic redispatch qualification stays separate.
