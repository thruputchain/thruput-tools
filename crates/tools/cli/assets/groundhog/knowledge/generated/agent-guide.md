---
name: groundhog
description: >
  Guides Groundhog contract and application development with the installed
  CLI, Rust SDK, generated TypeScript bindings, TypeScript client, wallets,
  local nodes, and JSON-RPC. Use when a task mentions Groundhog or a project
  contains Groundhog configuration, SDK dependencies, or generated artifacts.
---

<!-- groundhog-cli-version: 0.19.0 -->

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

This CLI uses SDK 0.41.1, standard interfaces 0.1.41, protocol-rs 0.42.0,
and XDR 0.10.1. Update the SDK and interface dependencies together when
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

A plain `groundhog build` warns when codegen would refuse the contract's spec,
naming the declarations to rename; with `--bindings ts` the build fails instead.

Generated method descriptors sit under their interface's name and major, as in
`ERC20.v0_1.transfer`, beside their `ERC20.v0_1.TransferArgs` and
`ERC20.v0_1.TransferResult` types and the interface's `DeclaredException` and
`Exception` types. They provide typed operations such as:

- `invocation(...)` to encode an application call;
- `call(...)` in the client DX runtime to retain typed result/exception decoders;
- `view(...)` and `viewOutcome(...)` for read-only execution;
- `decodeReturn(...)` and `decodeException(...)` for raw call data;
- `decodeReceipt(...)` and `decodeReceiptOutcome(...)` for a receipt whose root
  method is known.

Generated event descriptors such as `my_token.v1.events.Transfer` provide
`decode(...)`, `filter(...)`, `allFromReceipt(...)`, and
`singleFromReceipt(...)`.

Declared types, enums and events sit under their spec id's path: crate, major,
then module, as in `my_token.v1.errors.TokenError`. The same path names the
type and the value: types live in namespaces, and enum values and event
descriptors in one object per crate, so a binding is erasable TypeScript that
Node's type stripping runs without a build step. A path segment that is a
TypeScript reserved word or `globalThis`, and a crate or interface named
`spec` or `decodeEvent`, take a leading `$`, as in
`my_token.v1.$default.Config`.

Use those generated codecs. Do not create a low-level XDR reader when a binding
already knows the method or event type.

Bindings emit canonical interface/version and ABI/public method names for runtime
diagnostics. They require a client runtime accepting this method metadata. The
client DX runtime implements typed calls, execution and receipt observation;
codegen does not embed wallet or transport behavior. Hashed event topics contain
only the digest, not the original value; generated comments describe their
filtering restriction. Regenerate bindings together with the reviewed client
runtime before adopting the new descriptors.

`u32`, `u64`, `u128`, `i32`, `i64`, `i128`, `U256`, `I256` and `bool` topics are
directly decoded and filterable. Integer topics are fixed 32-byte big-endian
values with zero/sign extension, not compact XDR scalar encodings. Strings and
byte slices remain hash-only.

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

Generated methods sit under their interface's major, which the app selects once:

```ts
import { Counter as CounterVersions } from "./counter";
import { DefaultWallet as DefaultWalletVersions } from "@groundhogchain/client/contracts";

const Counter = CounterVersions.v0_1;
const DefaultWallet = DefaultWalletVersions.v0_1;
```

These constants hold only values, so name generated types by their full path,
as in `CounterVersions.v0_1.GetResult`.

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

For first-run setup in automation, use `groundhog init --non-interactive --json`.
It initializes global profiles and reports incomplete setup with exit 1 when
Cargo, the active profile, or a selected stored signing key is unavailable.
Add `--generate-key` only when authorized to create the initial signing key;
existing keys and explicit key selections are preserved. `--quiet` does not
accept wizard defaults. `init --json` performs setup; use `status --json` for
read-only diagnostics. Create scripted projects separately with `groundhog new`.

Use the CLI to start the supported local stack:

```console
groundhog node
```

The CLI can acquire a catalog-pinned devnode bundle on startup. Use
`groundhog toolchain list` to inspect the embedded catalog, `toolchain install`
to prefetch it, and `toolchain which` to find the installed executable.
`groundhog --offline node` forbids managed downloads; it does not disable RPC
or Cargo network traffic in other commands. Keep `GROUNDHOG_DEVNODE_BINARY`
for a matching local build. Diagnose startup compatibility failures rather
than adding a protocol override.

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

Use global `--non-interactive` to disable CLI prompts and terminal UI while
retaining ordinary output. It does not grant consent: protected actions still
require `--force`, and initial key creation requires `init --generate-key`.
`keys import` needs piped stdin when prompts are disabled; never place seeds
in arguments. Cargo/test subprocesses cannot read terminal stdin in this mode.

Use `--json` for agent-consumed command results. Respect commands that require
explicit confirmation, and never add a confirmation override unless the user
has authorized the consequential action.

A receipt timeout does not establish that a transaction failed. Inspect the
transaction using the command printed in the error before submitting the
operation again. An `unknown` status does not explain why the transaction is
absent. For detailed local backend logs, start `groundhog node --verbose`;
output goes to that terminal unless redirected. Do not restart an ephemeral
node just to enable logging: stopping it discards its chain state.

### Key and address interoperability

`groundhog keys import <name>` reads an Ed25519 seed from a hidden terminal
prompt or piped stdin. It accepts a checksummed Stellar S StrKey or a 32-byte
hex seed, never a 64-byte stored keypair. Do not put private seeds in command
arguments. Existing names are not overwritten; `dev-N` names are reserved.
Import JSON uses a versioned camelCase report (`data.schemaVersion = 1`).
Malformed TOML or an invalid `default_key` is rejected before storing the key.
If the key is stored but selecting the default fails, import succeeds with a
warning and a recovery
command (`groundhog keys use <name>`); do not import it again. `isDefault` is
null when the configuration cannot be reread to confirm selection.

`groundhog keys export <name> --format strkey` exports the same seed as an S
StrKey after the normal confirmation gate. The default export remains hex.
For an authorized non-interactive transfer between key stores, pipe export
stdout directly to the receiving CLI's stdin rather than capturing it in logs.

A G StrKey is an Ed25519 public key, not a derived Groundhog account or wallet
address. Key commands display public keys separately from account addresses.
C StrKeys and legacy hex are accepted for contract/wallet address inputs,
including typed contract arguments. Saved wallet owners accept G StrKeys or
hex public keys. These encodings preserve bytes and do not identify a network.
Existing key files and public JSON-RPC wire encodings remain unchanged.

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
