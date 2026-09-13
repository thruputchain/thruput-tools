---
name: groundhog
description: >
  Guides Groundhog contract and application development with the installed
  CLI, Rust SDK, generated TypeScript bindings, TypeScript client, wallets,
  local nodes, and JSON-RPC. Use when a task mentions Groundhog or a project
  contains Groundhog configuration, SDK dependencies, or generated artifacts.
---

<!-- groundhog-cli-version: 0.17.0 -->

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
other. The scaffolded SDK downloads its test executor from public storage; no
FFI download credentials are required. Cargo registry authentication follows
the early-access setup instructions. Use `groundhog test --integration` for
RPC tests against an ephemeral local node.

Preserve the scaffold's feature names, RV32 binary wrapper, Cargo overlays, and
self dev-dependency. They are toolchain contracts, not incidental style.

CLI 0.17 uses SDK 0.34.3, protocol-rs 0.35.2, and XDR 0.10.1. Update
these dependencies together when migrating an existing Rust project. Despite
its patch number, SDK 0.34.3 changes public XDR types. Standard interfaces
`=0.1.30` use this family while preserving the native token contract ABI;
projects retaining SDK `=0.34.2` should retain interfaces `=0.1.29`.

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

`groundhog build` compiles and lowers the deployable RV32 artifact. A contract
spec produced by the Rust contract is the canonical interface for generated
bindings.

The build is offline and checks the artifact against no chain. It reports the
code and guest data memory sizes, and warns when the code section is over the
protocol-default 64 KiB. Whether a chain admits the artifact is decided at
deploy: the RPC endpoint rejects a transaction whose artifact exceeds the
chain's `max_rv32_code_bytes` or `max_rv32_memory_bytes` before submitting
it, and `groundhog deploy` reports the violated bound. Guest data memory
defaults to 64 KiB. A contract that needs more sets `initial_memory_bytes`
under `[package.metadata.groundhog]` in its `Cargo.toml` (an integer, or a
string such as `"128K"`); `groundhog build` and `groundhog test` both read
it, size the linker layout from it, and stamp it into the artifact. Do not
raise it for a chain that has not raised `max_rv32_memory_bytes`.
`--stack-size` still adjusts the stack reserve per build.

Generate a TypeScript binding as part of the build:

```console
groundhog build --bindings ts
```

Or generate it from an existing spec:

```console
groundhog codegen ts path/to/contract.spec.xdr -o path/to/contract.ts
```

Run the installed commands with `--help` for current paths and options. Re-run
codegen after changing exported methods, events, exceptions, or contract types.
Do not hand-maintain a TypeScript ABI beside the Rust contract spec.

Generated method descriptors provide typed operations such as:

- `invocation(...)` to encode an application call;
- `view(...)` and `viewOutcome(...)` for read-only execution;
- `decodeReturn(...)` and `decodeException(...)` for raw call data;
- `decodeReceipt(...)` and `decodeReceiptOutcome(...)` for a receipt whose root
  method is known.

Generated event descriptors such as `PingEvent` provide `decode(...)`,
`filter(...)`, `allFromReceipt(...)`, and `singleFromReceipt(...)`.

Use those generated codecs. Do not create a low-level XDR reader when a binding
already knows the method or event type.

## TypeScript Application Path

Use the package's public exports and generated binding, not files below its
`src/` directory or dependencies below `node_modules`.

The application flow is:

1. Create a cluster-aware Groundhog client.
2. Create or load an account and signer appropriate for that network.
3. Bind the account to a wallet client.
4. Build an application invocation with the generated method descriptor.
5. Execute the invocation through the wallet.
6. Wait for the receipt.
7. Decode the wallet root outcome, then decode child return data when needed.

A read-only call goes through the generated descriptor:

```ts
const result = await Counter.get.view({
  client,
  contract,
  caller,
});
```

A state-changing call is built as an invocation and executed through a wallet:

```ts
const invocation = Counter.increment.invocation(contract, callGas, {
  amount: 1n,
});

const txHash = await wallet.execute({
  invocation,
  gasLimit: walletGas,
});

const receipt = await wallet.waitForTransactionReceipt(txHash);
```

Do not submit `Counter.increment` directly as the transaction root. A pubkey
account normally uses DefaultWallet. Local bootstrap source-address accounts
normally use DevWallet with the local-development adapter. Custom smart
wallets provide their own root invocation adapter.

For a DefaultWallet transaction, first decode the root wallet receipt:

```ts
const root = DefaultWallet.execute.decodeReceiptOutcome(receipt);
```

If the wallet returned raw child values, select the child corresponding to the
application invocation and decode it with that method's generated
`decodeReturn`. The receipt does not contain enough TypeScript method identity
for a generic decoder to choose the child descriptor automatically.

Submission, commitment, and successful contract execution are separate facts.
Check the typed receipt outcome before treating a state-changing operation as
successful. For dependent flows such as an airdrop followed by a transfer,
verify the first execution result before submitting the second operation.

## Local Development

Use the CLI to start the supported local stack:

```console
groundhog node
```

Use another terminal for status, build, deploy, call, view, or TypeScript
application work. Prefer the active local profile and runtime information
reported by `groundhog status --json` and `groundhog node --json`. Do not copy
deterministic local-development keys into production configuration.

Typical CLI discovery:

```console
groundhog status --json
groundhog build --help
groundhog test --help
groundhog node --help
groundhog deploy --help
groundhog call --help
groundhog view --help
groundhog codegen ts --help
```

Use `--json` for agent-consumed command results. Respect commands that require
explicit confirmation, and never add a confirmation override unless the user
has authorized the consequential action.

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
