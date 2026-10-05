+++
schema_version = 1
id = "groundhog.typescript-application"
owner = "groundhog-rpc"
kind = "workflow"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 50
+++
## TypeScript Application Path

Use public exports and generated bindings. Connect a cluster-aware client, load
an account, and bind a wallet. One client supports multiple wallets. Pubkey and
schema-3 bootstrap accounts use DefaultWallet. Historical source-address wallets
use `devWalletAdapter`; preserve their addresses. Never submit an application
invocation directly as the transaction root.

Select an interface version for values; explicit types use the full namespace:

```ts
import { Counter as CounterVersions } from "./counter";
const Counter = CounterVersions.v0_1;
type Result = CounterVersions.v0_1.GetResult;

const result = await Counter.get.view({ client, contract, caller, signal });
const invocation = Counter.increment.invocation(contract, callGas, { amount: 1n });
const execution = await wallet.executeAndWait({ invocation, gasLimit: walletGas });
```

`method.call` retains decoders for `executeCallsAndWait` on wallets that expose
child returns. For manual decoding, decode the known wallet root receipt first,
then each known child's return buffer. DevWallet discards child returns. Never
decode root abort bytes as a child's exception merely because there was one call.

Submission, commitment, and execution success differ. `executeAndWait` requires
success. `TransactionObservationError` retains the original hash; observe it on
the original network after uncertainty, without automatically re-signing.
Provider-backed helpers require `onSigned` to persist signed evidence before
broadcast. Explicit prepare/sign/persist/submit remains available.

### Amounts, reads, and diagnostics

- Common primitives and constructors are client root exports. Protocol-only
  symbols such as `HOG_ADDRESS` remain in the protocol package.
- `parseUnits("12.05", 2)` returns `1205n`; `formatUnits(1205n, 2)` returns
  `"12.05"`. Neither rounds. `ParseUnitsError` reports malformed input or excess
  precision. Applications check positivity and ABI bounds.
- ERC20 `adjustAllowance` takes a signed `difference` and an `exclusive` flag.
  Exclusive locking does not make an earlier off-chain read current; a read
  followed by a delta cannot guarantee an exact target allowance.
- `getContract({ binding: Counter, address: contract, client })` binds defaults.
  Pass `{ signal }` to actions and `signal` inside generated or bound view options.
- `classifyReadError(error, { signal })` returns transient, permanent, cancelled,
  or unknown without replacing errors. A bare `TypeError` is not proof of a network
  failure. Classification does not authorize transaction resubmission.
- `getVersion` returns nullable `releaseVersion` and `bundleManifestDigest`.
  Use `wireFormat` for compatibility; release identity is diagnostic.
- RPC wire 9 and CLI JSON use canonical decimal strings for all 64-bit-or-wider
  integers; TypeScript uses `bigint`, never `Number`. Smaller integers stay numbers.

### HTTP batching

`new HttpTransport(url, { batch: true })` enables batching, off by default, with
`{ maxSize: 100, waitMs: 0 }`. Servers cap arrays at 1,000. Match responses by ID.
RPC errors are per-call; HTTP and malformed-response failures affect the group.
No automatic retries. `timeoutMs` includes body reading. Cancellation is per-call.
Batching provides no shared snapshot, ordering, or atomicity. Await dependencies.

### Event pages and restart

`getContractEvents({ event, contractAddresses, topics, limit })` fetches one typed
page. `iterateEventPages(query, { signal })` yields raw pages on demand, without
prefetch, retries, polling, or persistence. Errors propagate. Break stops future
requests; abort cancels an in-flight read.

- Keep the network, target, and filters fixed. A cursor continues the original
  historical scan, not future blocks. Raw requests still require order and anchor.
- Wire `"0x"` is an empty `Uint8Array` in the client. Stop on an empty returned
  cursor, not an empty page.
- For ascending ingestion, start a new ascending scan with an empty cursor at
  the last stored row's height. Deduplicate by transaction hash and event sequence
  number (hash alone for refs). Save rows and progress together.
- Refresh descending history with an empty cursor and client `anchor: "end"`
  (raw RPC `atEnd`). Its last row is the oldest seen, not a forward checkpoint.
- `latestBlock` is the indexer head, not an application checkpoint. Check
  `oldestBlock` for lost history. Current indexers do not reliably reject corrupt
  cursors; do not silently restart.

See the [restart walkthrough](https://docs.groundhog.dev/tutorials/add-events-to-a-contract/#resume-without-missing-events).
