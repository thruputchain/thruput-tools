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

Use public package exports and generated bindings, not package internals. Select
an interface version for values; explicit types still use the full namespace:

```ts
import { Counter as CounterVersions } from "./counter";
const Counter = CounterVersions.v0_1;
type Result = CounterVersions.v0_1.GetResult;

const result = await Counter.get.view({ client, contract, caller, signal });
const invocation = Counter.increment.invocation(contract, callGas, { amount: 1n });
const execution = await wallet.executeAndWait({ invocation, gasLimit: walletGas });
```

Connect a cluster-aware client, create/load an account, then bind a wallet client.
One public client supports multiple independent wallet bindings. Pubkey accounts
use DefaultWallet; local source-address accounts use `devWalletAdapter`.
Do not submit an application invocation directly as the transaction root.

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
  symbols such as HOG_ADDRESS remain in the protocol package.
- `parseUnits("12.05", 2)` returns `1205n`; `formatUnits(1205n, 2)` returns
  `"12.05"`. No floating point or rounding. ParseUnitsError rejects malformed
  input/excess precision. Applications check positivity and ABI bounds.
- `getContract({ binding: Counter, address: contract, client })` binds defaults.
  Pass `{ signal }` to actions and `signal` inside generated/bound view options.
- `classifyReadError(error, { signal })` returns transient, permanent, cancelled,
  or unknown without replacing errors. A bare TypeError is not proof of a network
  failure. Classification does not authorize transaction resubmission.
- `getVersion` returns nullable releaseVersion/bundleManifestDigest diagnostics.
  Use wireFormat for RPC compatibility decisions.

### HTTP batching and pages

`new HttpTransport(url, { batch: true })` groups concurrent requests; defaults are
`{ maxSize: 100, waitMs: 0 }`. It is off by default. Both supported servers cap
arrays at 1,000 entries. IDs correlate out-of-order responses; RPC errors remain
per-call. Shared HTTP/malformed-response failures affect the group. No automatic
retry occurs. timeoutMs includes body reading after dispatch. One cancellation
does not cancel neighboring calls. HTTP grouping provides no shared snapshot,
execution order, or atomicity. Await dependent operations separately.

`getContractEvents({ event, contractAddresses, topics, limit })` fetches one typed
page. `iterateEventPages(query, { signal })` yields raw pages on demand, without
prefetch, retries, polling, or persistence. Errors propagate. Break stops future
requests; abort cancels an in-flight read.

A cursor belongs to the same network/target/filters and one bounded historical
walk, not future blocks. Raw continuations still require order and anchor. Wire
`"0x"` is an empty Uint8Array in the client. Stop on an empty returned cursor,
not an empty page. For later scans, reread the last stored row's height and
deduplicate by tx hash plus event sequence number (hash alone for transaction
refs). Save rows/progress together; never skip a partially processed block.
latestBlock is the indexer head, not an application checkpoint. Check oldestBlock
for lost history. Current indexers do not reliably reject corrupt cursors; do not
silently restart. The site's events tutorial explains durable restart.
