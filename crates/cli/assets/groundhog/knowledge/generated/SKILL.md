---
name: groundhog
description: >
  Guides Groundhog contract and application development with the installed
  CLI, Rust SDK, generated TypeScript bindings, TypeScript client, wallets,
  local nodes, and JSON-RPC. Use when a task mentions Groundhog or a project
  contains Groundhog configuration, SDK dependencies, or generated artifacts.
---

<!-- groundhog-cli-version: {{GROUNDHOG_CLI_VERSION}} -->

# Groundhog

Groundhog is not EVM-compatible. Do not infer its execution, storage, account,
transaction, gas, or ABI behaviour from Ethereum, Solana, or another chain.
Use the installed CLI and the project's public Groundhog dependencies as the
authority for this release.

## Start Here

Before editing a Groundhog project:

1. Read the project's `AGENTS.md` and `groundhog.toml` when present.
2. Run `groundhog --version` and the relevant command's `--help`.
3. Run `groundhog status --json` when environment, toolchain, project, profile,
   key, or local-node state matters.
4. Identify which Groundhog layer owns the task before reading source.
5. Prefer public APIs, generated bindings, and current examples. Inspect
   `node_modules`, Cargo registry source, or Groundhog implementation
   repositories only after those public surfaces are insufficient.

Do not guess an installed command's syntax from this skill. The installed
binary's `--help` output wins when it differs.

## Know the Layers

| Layer | Owns |
|---|---|
| Rust contract | Methods, events, exceptions, authorization, and persistent state |
| Rust SDK | Contract macros, storage types, generated contract clients, and native testing |
| Contract build | The deployable `.rv32a` artifact and canonical `.spec.xdr` interface |
| Generated TypeScript binding | Typed method and event descriptors derived from `.spec.xdr` |
| TypeScript client | RPC transport, clusters, accounts, wallets, transactions, receipts, and events |
| Wallet | The authorized root invocation around one or more application calls |
| Groundhog CLI | Scaffolding, builds, tests, local nodes, keys, deployment, calls, inspection, and codegen |
| JSON-RPC | The public network boundary used by the CLI and TypeScript client |

Keep these boundaries explicit. In particular:

- A generated TypeScript binding describes a contract. It is not a connected
  contract instance and its methods are not direct RPC functions.
- The application invocation is normally a child of a wallet root invocation.
- The CLI and TypeScript client consume the public RPC API. Do not bypass them
  by constructing internal protocol messages unless the task specifically
  requires protocol work.

## Programming Model

All transactions considered for a block read the same frozen committed
snapshot. A transaction does not see another transaction's writes from that
block. Contract execution produces typed storage deltas rather than a bag of
arbitrary final writes.

After the root call finishes, the runtime checks whether its complete delta
batch can coexist with transactions already admitted to the proposal.
Compatible deltas can land together. A transaction with incompatible deltas is
excluded and returned to the mempool. Do not add retry loops inside contract
code to handle this exclusion.

Gas meters computation and bounds cross-contract call depth. It has no token
price and is not burned. Choose storage operations for correct conflict
semantics, not gas-price optimisation.

Declared root gas reserves scheduling capacity against the sender's free HOG;
target storage funding is separate. Use workload-tested budgets. A sufficient
first HOG credit can fund a recipient's balance-item deposit; a small first
credit may fail. Do not hard-code a universal deposit or require an airdrop for
every new address.

### Start with Balance, Then Choose by Behaviour

For contract authors, `Balance` is the practical starting point for ordinary
nonnegative amounts such as token balances, allowances, supply, deposits, and
credits. It is backed by Groundhog's mergeable integer storage, but presents a
zero-defaulting `u128` API that prevents negative values and absolute
replacement. Do not use raw `NNInt` for a balance unless the data model
genuinely needs its lower-level signed-delta or explicit-base operations.

- `Balance` is the ordinary type for balances, allowances, and supply. It uses
  `u128`, defaults missing state to zero, exposes increases and decreases, and
  does not expose absolute replacement or negative values.
- `NNInt` is the lower-level integer type for counters and state that needs
  signed deltas or an explicit base. Prefer `strict_inc` when execution must
  observe an immediate bound failure. Use `inc` only when deferring the bound
  check to reservation is intentional.
- `Map<K, V>` gives each key an independent conflict domain. The value type
  determines same-key behaviour.
- `Value<T>` stores typed configuration or other infrequently replaced values.
  It is usually wrong for hot counters or balances.
- `Vector<S>` serializes changes to its length. Do not assume concurrent pushes
  form a scalable shared log.
- `Bytestring` is raw byte storage for lower-level or infrequently replaced
  data. Prefer typed `Value<T>` when a typed value is available.
- `RCQ<T>` is the SDK's retro-causal queue. Within a block, submission order
  determines whether a consume sees another transaction's insert, and
  concurrent consumes conflict. Use it only when those semantics match the
  application.

For a token, use `Map<ContractAddress, Balance>` for per-account balances. For
a counter, use `NNInt`. Do not implement either as read, calculate, and replace
through `Value` or `Bytestring`.

## Rust Contract Path

Start from the installed scaffold instead of recreating its Cargo and RV32
configuration:

```console
groundhog new my-contract
cd my-contract
cargo test
groundhog build
groundhog test
```

Use `cargo test` for fast native tests. Use `groundhog test` for the generated
RV32 artifact. They exercise different paths and neither substitutes for the
other. The scaffolded SDK downloads its test executor from public storage.
Building a scaffold needs Cargo credentials for the Groundhog registry; the
Getting Started guide covers that setup. Use `groundhog test --integration` for
RPC tests against an ephemeral local node. In a multi-contract Cargo workspace,
`groundhog test --workspace` runs the RV32 tests of every member that has a
`tests/test_rv32.rs` target and reports them together.

Preserve the scaffold's feature names, RV32 binary wrapper, Cargo overlays, and
self dev-dependency. They are toolchain contracts, not incidental style.

That self dev-dependency puts the project name in the extern prelude of every
test target, so `groundhog new` refuses names that would shadow something the
generated project needs: Rust's built-in crates (`test`, `core`, `std`,
`alloc`, `proc-macro`), Rust keywords, the scaffold's own dependencies (`sdk`,
`tokio`, `groundhog-json-rpc-client`), Cargo's build directory names (`build`,
`deps`, `examples`, `incremental`), and reserved Windows device names. The
error names the collision; pick another name rather than patching the
generated manifest.

This CLI uses SDK 0.42.2, standard interfaces 0.1.44, protocol-rs 0.43.0,
and XDR 0.10.2. Update the SDK and interface dependencies together when
migrating an existing Rust project.

The normal starter contract is a counter, one of the narrower cases where
`NNInt` is appropriate. Do not copy this storage choice for token balances or
other ordinary nonnegative amounts; use `Balance` for those.

The contract shape is:

```rust
use sdk::storage::NNInt;
use sdk::*;

#[derive(Debug)]
#[sdk::storage]
pub struct Counter {
    value: NNInt,
}

#[contract]
impl Counter {
    pub fn increment(&mut self, amount: u64) {
        self.value.strict_inc(amount as i128);
    }

    pub fn get(&self) -> u64 {
        self.value.try_get().none_if_missing().unwrap_or(0) as u64
    }
}
```

Keep contract logic in `src/implementation.rs`; keep `src/lib.rs` focused on
the `no_std` gate, mock registration, module declaration, and public
re-exports. Prefer `#[contract_exception]` for contract-facing exceptions.
Update native and RV32 tests with behaviour changes.

Before inventing a macro attribute or test helper, inspect the scaffold and the
public Rustdoc for the SDK version the project actually uses.

## Build Artifacts and TypeScript Codegen

The Rust contract's `.spec.xdr` is the canonical interface. Build deployable RV32
code and generate bindings together, or generate from an existing spec:

```console
groundhog build --bindings ts
groundhog codegen ts path/to/contract.spec.xdr -o path/to/contract.ts
```

Use installed `--help` for paths/options. Regenerate after API changes; never
maintain a second TypeScript ABI. Plain build warns on codegen-incompatible names;
`--bindings ts` makes them errors. Rename the reported declarations.

Build is offline. It reports code/memory sizes and warns above the default 256 KiB
code limit; deployment enforces max_rv32_code_bytes and max_rv32_memory_bytes.
Guest memory defaults to 64 KiB. Set `initial_memory_bytes` in
`[package.metadata.groundhog]` (integer or `"128K"`) within the chain's limit.
Build/test use it for linker layout and artifact metadata. `--stack-size` sets a
reserve within that memory, not a larger total allowance.

Methods and types use interface/version paths: `ERC20.v0_1.transfer` and
`ERC20.v0_1.TransferArgs`. Descriptors provide:

- `invocation` and `call` to prepare calls;
- `view` and `viewOutcome` for simulation;
- `decodeArgs`, `decodeReturn`, and `decodeException` for known method data;
- `decodeReceipt` and `decodeReceiptOutcome` for a known root method's receipt.

`decodeArgs` consumes the complete input and returns undefined for no arguments.
It verifies byte shape, not contract behavior. This runtime addition needs no new
metadata; the separate spec/namespace migration requires regenerated bindings.

Generated event descriptors such as `my_token.v1.events.Transfer` provide
`decode`, `filter`, `allFromReceipt`, and `singleFromReceipt`. Types, enums, and
events follow the spec id: crate, major, module, declaration. Value objects and
type namespaces share paths, preserving erasable TypeScript and Node type
stripping. Reserved path names, globalThis, and crate/interface names spec or
decodeEvent take a leading `$`. Use generated codecs rather than manual XDR.

Addresses, fixed 32-byte values, `u32`, `u64`, `u128`, `i32`, `i64`, `i128`,
`U256`, `I256`, and `bool` are directly filterable topics. Integers use 32-byte
big-endian values with zero/sign extension, not CXS scalar encoding. Strings
and byte slices expose only a hash, not their preimage.

## TypeScript Application Path

Use public package exports and generated bindings, not package internals.
Connect a cluster-aware client, create or load an account, then bind a wallet.
One public client supports multiple independent wallet bindings. Pubkey accounts
use DefaultWallet, including schema-3 bootstrap accounts. Historical saved
legacy source-address wallets use `devWalletAdapter`; preserve their addresses.
Do not submit an application invocation directly as the transaction root.

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

### HTTP batching

`new HttpTransport(url, { batch: true })` groups concurrent requests; defaults are
`{ maxSize: 100, waitMs: 0 }`. It is off by default. Both supported servers cap
arrays at 1,000 entries. Match responses by ID; RPC errors remain per-call.
Shared HTTP or malformed-response failures affect the group. No automatic
retry occurs. `timeoutMs` includes body reading after dispatch. One cancellation
does not cancel neighboring calls. HTTP grouping provides no shared snapshot,
execution order, or atomicity. Await dependent operations separately.

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

## Local Development

Use `groundhog init --non-interactive --json` for scripted setup. Missing Cargo,
profile, or stored key exits 1. Key creation requires authorized `--generate-key`;
existing selections are preserved. `--quiet` does not grant consent. `init` writes;
`status --json` is read-only. Create projects with `groundhog new`.

Start the local stack with `groundhog node`. Inspect, prefetch, or locate its
catalog-pinned bundle with `toolchain list`, `toolchain install`, and `toolchain
which`. `--offline node` forbids managed downloads, not RPC/Cargo elsewhere.
`GROUNDHOG_DEVNODE_BINARY` selects a matching build. Diagnose compatibility
failures; do not override protocols. Use `status --json` / `node --json` for runtime
information and `--help` for flags. Never use deterministic local keys in production.

`--non-interactive` disables prompts/UI without granting consent: protected
operations require authorized `--force`. Key imports need piped stdin; Cargo/test
subprocesses cannot read terminal stdin. Use `--json` for machine results.

`--no-ui` selects plain output; `NO_COLOR` disables color. Explicit live-output
modes stream immediately without animation.

Hosted signing can discover an unconfigured network hash from matching descriptor
and RPC identities. Configured hashes remain enforced.

Timeout or `unknown` does not prove transaction failure. Inspect the hash before
retrying. `node --verbose` enables backend logs; restarting an ephemeral node
loses its state.

`events list`, `tx list`, and `token transfers` read newest-first pages.
Omit `--cursor` to refresh from the head; a continuation goes toward older rows.
Continue with the returned opaque cursor and the same target/filters. `tx list`
matches root invocation targets, not every address touched by a transaction.
See [Event pages and restart](#event-pages-and-restart) for cursor rules.

### Key and address interoperability

`groundhog keys import <name>` reads a seed from hidden input or piped stdin:
a checksummed S StrKey or 32-byte hex, never a 64-byte keypair or CLI argument.
Existing names are not overwritten; `dev-N` names are reserved.
Import JSON uses a versioned camelCase report (`data.schemaVersion = 1`).
Malformed TOML or an invalid `default_key` is rejected before storing the key.
If saving succeeds but default selection fails, use `groundhog keys use <name>`;
do not reimport. `isDefault` is null if configuration cannot be reread.

`groundhog keys export <name> --format strkey` exports the same seed as an S
StrKey after the normal confirmation gate. The default export remains hex.
For an authorized non-interactive transfer between key stores, pipe export
stdout directly to the receiving CLI's stdin rather than capturing it in logs.

A G StrKey is an Ed25519 public key, distinct from an account or wallet address.
C StrKeys and legacy hex are accepted for contract/wallet address inputs,
including typed contract arguments. Saved wallet owners accept G StrKeys or
hex public keys. These encodings preserve bytes and do not identify a network.
Existing key files and public JSON-RPC wire encodings remain unchanged.

### Native accounts

Ordinary keys derive nonce-zero native accounts. `wallet create` saves the empty
identity offline; fund explicitly with `airdrop`. `--prefund` is rejected. Saved
legacy wallets retain identity and format. Schema-3 bootstrap dev-N accounts
are native pubkey accounts with nonce zero, using the advertised seed and keys.
Use a matching native runtime; schema-2 state needs its original runtime.

Native deploy requires `--fund <HOG>` and hogd 0.1.29-rc.2 or later. The exact
amount is not an estimate; zero skips transfer. Add `--init <METHOD>` and repeated
`--init-arg <VALUE>` using the artifact spec, or a 0x selector and raw hex argument.
`--init-gas-limit` sets child gas. Init runs as the wallet, discards its return,
and rolls back with funding/deployment on failure. Dry-run submits nothing.
Unverified results retain signed identity: inspect before retrying because nonce
and address change. Aliases require verified success at the predicted address.

## Sources and Escalation

Use this source order:

1. Project `AGENTS.md`, manifests, generated artifacts, and current code.
2. Installed `groundhog` help and structured output.
3. Public SDK Rustdoc, TypeScript declarations, generated bindings, and
   Groundhog developer documentation matching the installed versions.
4. Current blessed examples.
5. Public implementation source when a documented contract is incomplete.
6. Dependency internals only when debugging the dependency itself or when the
   public contract cannot answer the question.

When sources disagree, stop and identify the versions and exact disagreement.
Do not silently combine APIs from different Groundhog releases. Prefer the
installed scaffold and the public dependencies selected by that project.

Never print, log, store, or paste private signing material. Use public
addresses, transaction hashes, and redacted configuration when reporting a
problem.
