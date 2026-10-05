---
name: groundhog
description: >
  Guides Groundhog contract and application development with the installed
  CLI, Rust SDK, generated TypeScript bindings, TypeScript client, wallets,
  local nodes, and JSON-RPC. Use when a task mentions Groundhog or a project
  contains Groundhog configuration, SDK dependencies, or generated artifacts.
---

<!-- groundhog-cli-version: {{GROUNDHOG_CLI_VERSION}} -->
<!-- skill-source: https://github.com/groundhogchain/agent-skills; revision: 03e7080a80be91dfb83a17e3f626636516819cd8; sha256: sha256:c001854d81d003d6bc9c9bec1e884d37d5a8da00ef3bae3ba6215b5e7eeb614a; dirty: false -->

# Groundhog

Groundhog is not EVM-compatible. Do not infer its execution, storage, account,
transaction, gas, or ABI behavior from Ethereum, Solana, or another chain.

## Start here

1. Read the project's `AGENTS.md` and manifests when present.
2. Identify the task and read the relevant reference below.
3. If the task uses the CLI, run `groundhog --version` and the relevant
   command's `--help`. Use `groundhog status --json` when local setup matters.
4. Prefer public APIs, generated bindings, and current examples. Inspect
   `node_modules`, Cargo registry source, or Groundhog implementation
   repositories only after those public surfaces are insufficient.

Application reads do not require a Rust toolchain, wallet, or local node.
Use the dependencies and network selected by the project.

## Choose a reference

| Task | Read | Bundled CLI access |
| --- | --- | --- |
| Contract logic, storage, native and RV32 tests | [Contracts](#contracts) | `groundhog skill show contracts` |
| Generated bindings, application reads, wallet execution, events | [TypeScript](#typescript) | `groundhog skill show typescript` |
| React application state and reads | [React](#react) | `groundhog skill show react` |
| CLI setup, keys, wallets, deployment, local nodes | [CLI](#cli) | `groundhog skill show cli` |
| Errors, version disagreements, uncertain submissions | [Troubleshooting](#troubleshooting) | `groundhog skill show troubleshooting` |

Read references as needed. Use `groundhog skill show --all` when you need
the complete bundled guide.

## Shared rules

- A generated TypeScript binding describes a contract. It is not a connected
  contract instance and its methods are not direct RPC functions.
- Application invocations normally execute as children of a wallet root
  invocation. Use the public client or CLI to prepare and submit transactions.
- Submission, commitment, and successful execution are different states.
  After a timeout, follow [transaction inspection](references/troubleshooting.md#inspect-the-original-transaction).
- Gas meters computation. It has no token price and
  is not burned. Declared root gas reserves scheduling capacity against free
  HOG; storage funding is separate.
- Never print, log, or paste private signing material. Keep private keys in the
  wallet or designated key store. Persist signed transaction evidence before
  broadcast, as shown in the TypeScript example. Use public
  addresses, transaction hashes, and redacted configuration in reports.

## Documentation and versions

Start at <https://docs.groundhog.dev/> for product documentation. The installed
CLI's `groundhog docs` serves its selected documentation snapshot locally.

The installed skill follows the latest maintained source. CLI-bundled guidance
is selected separately for that CLI release. Neither overrides the project's
installed API. Each example names its dependency set. Start from that complete
example, then check the project's installed declarations if its versions differ.

<!-- references/cli.md -->

# CLI

This reference covers Groundhog CLI 0.23.0 with schema-4 local manifests and
RPC wire format 10. Run `groundhog --version` and the command's `--help` to
check the installed build; these contracts can change independently of the
CLI package version.

## Local development

```sh
groundhog init
groundhog new my-counter
cd my-counter
cargo test
groundhog build
groundhog test
```

`init` configures the toolchain, profile, and key selection. It can prompt for
registry access. `status --json` inspects setup without changing it. Keep the
generated Cargo configuration and RV32 wrapper.

Start `groundhog node` in a separate terminal. It selects a compatible managed
runtime. Use `groundhog toolchain list --json` to inspect available versions and
installation state. `GROUNDHOG_DEVNODE_BINARY` is for an explicit compatible
runtime build. A compatibility error needs diagnosis, not a protocol override.

An ephemeral node loses state when restarted. Use the CLI's persistent-node
options when the application needs its deployments across restarts. Read the
[local-node guide](https://docs.groundhog.dev/cli/local-node/).

Local accounts use ordinary stored keys. `groundhog node --key alice` gives
that key's nonce-zero native account an initial allocation of 100 HOG. With no
selected key, the node starts only its random private faucet. Names such as
`dev-1` are ordinary key names, not implicit local bootstrap identities.

Schema-4 local manifests contain public allocations, not user signing seeds.
Harnesses retain their own fixture signers and pass public allocations through
`--bootstrap-accounts FILE`. Never use fixture keys in production. Persistent
chain state and dumps retain the private faucet and its funding journal; older
local state requires its original runtime and CLI. Hosted schema-3 bootstrap
derivation is separate from local accounts.

For uncertain local funding, retain the original recipient, amount, and request
nonce. Retry the same request rather than creating a second funding operation.
Inspect any reported transaction hash on the original network.

## Keys and wallets

Keep private keys in the CLI key store. `groundhog keys import <name>` accepts a
seed through hidden input or stdin, not a command-line argument. A public key is
not a wallet address; inspect the selected account with the wallet commands.

Wallet creation, selection, address inspection, and status ownership checks use
cached public metadata without loading the private signer. Older key files may
derive missing metadata from the stored keypair. Transaction commands still load
the signer and verify wallet ownership.

`groundhog wallet create` saves an account identity. Fund it explicitly with
`groundhog airdrop` on a test network. Select the intended profile on commands
that access the network. Never use deterministic local test keys outside a local
test environment.

For simulated caller checks, `groundhog view --from ADDRESS` supplies the caller.
It also accepts saved wallets, stored keys, and contract aliases. Saved wallets
take precedence and retain their native nonce or legacy address. Omitting
`--from` leaves the simulated caller unset. A selected signing wallet does not
implicitly become the view caller.

## Deploy and initialize

Use `groundhog deploy --help` for the artifact argument and network options.
Deployment requires an explicit `--fund <HOG>` storage-funding amount; zero skips
the transfer. The CLI does not estimate this amount.

Add `--init <METHOD>` and repeated `--init-arg <VALUE>` to initialize atomically
with funding and deployment. The contract spec supplies argument types.
`--init-gas-limit` sets the child-call budget. The initializer runs as the wallet;
failure rolls back the operation. A dry run submits nothing.

Keep the original hash and predicted address if confirmation fails. Follow
[transaction inspection](#inspect-the-original-transaction)
before creating another deployment.

## Inspect transactions and events

```sh
groundhog tx show TX_HASH --profile local --json
groundhog events list CONTRACT_ADDRESS --profile local --limit 25 --json
```

Replace the uppercase values with the original hash and contract address. Use
the transaction's original profile/network. `tx show` performs one lookup and
fetches a receipt when committed; it does not wait or resubmit.

An indexed committed record takes precedence over scheduler outcomes. A
`committed` status can omit height until indexing catches up. `retry` means
resubmitting the same transaction may succeed; the mempool does not retry it
automatically. `dropped` requires changing the transaction before resubmission.
Use the reported error source and code to diagnose the cause.

`events list`, `tx list`, and `token transfers` return newest-first pages.
Continue with the returned cursor and the same target/filters; omit the cursor
to refresh from the head. Address transaction history matches root targets,
which can be wallets rather than the application contract. Absence from such a
list does not prove that a call failed.

## Automation

Use `--json` for structured results and `--non-interactive` to disable prompts.
Non-interactive mode does not grant consent: key generation needs
`--generate-key`, and protected actions need an explicitly authorized `--force`.
`--quiet` only controls output. Never collect private-key exports in logs.

Use the [diagnostic table](troubleshooting.md) when setup or execution fails.

<!-- references/contracts.md -->

# Contracts

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

Gas meters computation. It has no token
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
zero-defaulting `i128` API with values in `0..=i128::MAX`, without absolute
replacement. Do not use raw `NNInt` for a balance unless the data model
genuinely needs its lower-level signed-delta or explicit-base operations.

- `Balance` is the ordinary type for balances, allowances, and supply. It uses
  nonnegative `i128` amounts, defaults missing state to zero, and exposes
  increases and decreases. It rejects negative amounts and has no absolute set.
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
- `RCQ<T>` is the SDK's retrocausal queue for concurrent request insertion and
  ordered processing. Use the [AMM guide](https://docs.groundhog.dev/tutorials/build-an-amm/)
  for a complete queued application rather than assuming ordinary map semantics.

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

The current CLI 0.23.0 scaffold uses SDK 0.44.2, standard interfaces 0.1.47,
protocol-rs 0.46.0, and XDR 0.10.3. Check the generated manifest for the installed
build. Update SDK and interface dependencies together when migrating a project.

## Complete Balance example

Start from the maintained
[ERC-20 contract](https://github.com/groundhogchain/groundhog-examples/tree/main/contracts/erc20)
for a complete Balance-based implementation, including authorization, transfers,
allowances, events, and tests. The source checked for this skill is
[revision efb4250](https://github.com/groundhogchain/groundhog-examples/tree/efb425068d16b10fbb25c9bb980115737206a7e6/contracts/erc20),
using the older CLI 0.22.1 dependency set: SDK 0.42.2, standard interfaces
0.1.44, protocol-rs 0.43.0, and XDR 0.10.2. Treat this as a versioned example;
check the project's installed APIs before adapting it to newer dependencies.

Read `src/implementation.rs` for `Map<ContractAddress, Balance>` and
`tests/common/contract_test.rs` for transfer and bound checks. The checked debit
uses `try_decrease`; the recipient credit uses `try_lazy_increase`. Matching
storage operations can merge only when aggregate bounds and the transaction's
other changes also permit it. Follow the repository's setup instructions and run
both its native and RV32 tests before changing the implementation.

The normal starter contract is a counter, one of the narrower cases where
`NNInt` is appropriate. Do not copy this storage choice for token balances or
other ordinary nonnegative amounts; use `Balance` for those.

Keep contract logic in `src/implementation.rs`; keep `src/lib.rs` focused on
the `no_std` gate, mock registration, module declaration, and public
re-exports. Prefer `#[contract_exception]` for contract-facing exceptions.
Update native and RV32 tests with behaviour changes.

Use the scaffold and the public Rustdoc for the project's SDK version when
looking up a macro attribute or test helper.

<!-- references/react.md -->

# React

## Read a contract

Verified on 2026-10-05 with the published compatible pair `@groundhogchain/react` 0.3.1
and `@groundhogchain/client` 0.11.0, with protocol 0.12.0, React/React DOM 19.1.1,
and TanStack Query 5.103.1. The exact example passed type checks and local-node
browser checks for reads, refresh, and errors using CLI 0.22.1.
The React package requires React 19 and TanStack Query
5.90.1 or newer within v5. Keep its client peer requirement when installing.

In a Vite React TypeScript app, configure
[GitHub Packages access](https://github.com/groundhogchain/client-ts#registry-auth-one-time-setup)
and install:

```sh
npm install --save-exact @groundhogchain/react@0.3.1 @groundhogchain/client@0.11.0 @groundhogchain/protocol@0.12.0 @tanstack/react-query@5.103.1 react@19.1.1 react-dom@19.1.1
```

Start `groundhog node` in another terminal. Keep the app's `<div id="root"></div>`
and configure a same-origin development proxy in `vite.config.ts`:

```ts title="vite.config.ts"
import { defineConfig } from "vite";

export default defineConfig({
  server: { proxy: { "/rpc": { target: "http://127.0.0.1:9545", rewrite: () => "/" } } },
});
```

Replace `src/main.tsx` with the following. No wallet connection or private key
is needed for this read. For a hosted network, use that network's configured
cluster instead of `localnet()`.

```tsx title="main.tsx"
import { createRoot } from "react-dom/client";
import { createGroundhogClient, formatUnits, localnet } from "@groundhogchain/client";
import { ERC20 } from "@groundhogchain/client/contracts";
import { HOG_ADDRESS } from "@groundhogchain/protocol";
import { WalletProvider, useContractView } from "@groundhogchain/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

function HogSupply() {
  const supply = useContractView({
    method: ERC20.v0_1.totalSupply,
    contract: HOG_ADDRESS,
  });
  return (
    <section>
      <h1>HOG supply</h1>
      {supply.isLoading ? <p role="status">Loading supply…</p> : null}
      {supply.error ? <p role="alert">{supply.error.message}</p> : null}
      {supply.data ? (
        <>
          <p>{formatUnits(supply.data.value, 7)} HOG</p>
          <p>Read at block {supply.data.height.toString()}</p>
        </>
      ) : !supply.isLoading && !supply.error ? <p>No result yet.</p> : null}
      <button disabled={supply.isFetching} onClick={() => void supply.refetch()}>
        {supply.isFetching ? "Reading…" : "Refresh"}
      </button>
    </section>
  );
}

const element = document.getElementById("root");
if (!element) throw new Error("Missing root element");
const root = createRoot(element);

async function main() {
  const client = await createGroundhogClient({
    cluster: localnet({ rpcUrl: new URL("/rpc", window.location.origin).href }),
  });
  const queries = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  root.render(
    <QueryClientProvider client={queries}>
      <WalletProvider client={client}>
        <HogSupply />
      </WalletProvider>
    </QueryClientProvider>,
  );
}

main().catch((error: unknown) => {
  root.render(<p role="alert">{error instanceof Error ? error.message : "Connection failed"}</p>);
});
```

Run the app's `npm run dev`. Expect a supply and block height. Refresh performs
another view. The screen shows connection and query errors; during a failed
refresh, any retained data still has its original read height.

`WalletProvider` supplies the public client even while disconnected.
`QueryClientProvider` supplies the cache required by read and execution hooks.
Both clients are created once before rendering. The hook forwards cancellation
and keeps quantities as `bigint` until formatting.

## Add writes

Connect through `useWallet()` and enable writes only while its status is
`connected`. Use `useExecute` or `useWalletClient()` from an explicit user action.
Persist signed evidence in `onSigned` before broadcast. Keep its original network,
account, and hash after errors or disconnect; use `useWaitForTransactionReceipt`
to resume observation without requesting a new signature.

A committed execution failure is receipt data, not a failed query. Inspect
`execResult`. See the [wallet persistence example](#wallet-write-with-recovery)
for the storage and recovery boundary, using the client version selected by this
React package.

- [Package setup and wallet usage](https://github.com/groundhogchain/react#configure-the-application)
- [Contract reads and execution guide](https://github.com/groundhogchain/react/blob/main/docs/contract-transactions.md)
- [React documentation](https://docs.groundhog.dev/react/)

<!-- references/troubleshooting.md -->

# Troubleshooting

Record the command, exit code, exact error, selected network, CLI version, and
relevant dependency versions. Include public addresses and hashes, but redact
tokens and private keys. Commands below use Groundhog CLI 0.23.0; the status
table describes RPC wire format 10.

## Diagnose the symptom

| Symptom | Evidence to collect | Next action |
| --- | --- | --- |
| npm or Cargo registry authentication fails | Registry hostname, HTTP status, package name/version; `npm config get @groundhogchain:registry` for npm; the registry URL selected by Cargo | Configure credentials for that registry using [client setup](https://github.com/groundhogchain/client-ts#registry-auth-one-time-setup) or [CLI setup](https://docs.groundhog.dev/get-started/cli/). Confirm package access. Do not print credential files or replace private dependencies with similarly named public packages. |
| Runtime or network descriptor is incompatible | `groundhog --version`, `groundhog status --json`, `groundhog toolchain list --json`, and the exact expected/received values; note any explicit runtime override | For a local node, use the CLI-selected runtime or a source-verified compatible override. For a hosted endpoint, report the mismatch to its operator. Preserve persistent node data while investigating. |
| Generated binding and client disagree | `npm ls @groundhogchain/client @groundhogchain/protocol @groundhogchain/react`; CLI version; compiler error; `.spec.xdr` from the deployed build | Install compatible dependencies and regenerate with `groundhog codegen ts CONTRACT.spec.xdr -o src/contract.ts`. Do not edit generated types or suppress peer errors. Use the [read example](#read-a-contract) as a baseline. |
| Submission or observation times out | Original hash, network, signed evidence if saved, error cause and submission cause | Run the inspection procedure below. Keep observing the original hash. A timeout stops observation, not execution; do not sign a replacement automatically. |
| Transaction committed but execution failed | Receipt, execution result, revert data if present, root wallet method, intended child calls, gas limit and gas used | Fix the reported authorization, balance, storage-funding, or execution-budget condition. Decode the known root first; root abort bytes are not automatically a child exception. A new user-authorized attempt is a new transaction. |
| Execution succeeded but return decoding failed | Successful receipt, generated binding, deployed spec, and decoder error | Correct the decoder or binding and decode the same receipt again. Do not repeat a successful write to obtain its return value. |

## Inspect the original transaction

1. Recover the hash from the submission result, `TransactionObservationError`,
   or the saved `onSigned` record. Keep its original account and network.
2. Inspect the CLI profile's endpoint and identity. Use the profile that matches
   the saved network, not whichever profile is currently selected:

   ```sh
   groundhog config get local --sources
   groundhog tx show TX_HASH --profile local --json
   ```

   Replace `local` with the original profile and `TX_HASH` with the original
   32-byte hash. Do not send a seed or signed transaction to this command.
   If the application has no CLI profile, use its public client and
   `inspectPending()` below rather than selecting an unrelated profile.
3. Read the status and, when available, the receipt's execution result:

   | Result | Next action |
   | --- | --- |
   | `pending` | Keep the hash and repeat the read-only lookup later. |
   | `committed` with successful execution | Record the receipt height and refresh application reads. |
   | `committed` with failed execution | Keep the receipt and diagnose its execution result. |
   | `recentFailure` | Treat it as an advisory attempt result; the transaction may still commit. |
   | `retry` | Inspect the error source and code. Resubmitting the same transaction may succeed; the mempool does not retry it automatically. |
   | `dropped` | Inspect the error source and code. Change the transaction before an authorized resubmission. |
   | `invalid` | Inspect the reason. Do not hide it behind an automatic new signature. |
   | `unknown`, `other`, missing receipt, or lookup error | The outcome is unresolved. Check endpoint identity and availability, preserve evidence, and inspect again. |

An indexed committed record takes precedence over scheduler outcomes. A
`committed` status can omit height while indexing catches up; do not interpret
a missing height as a failed transaction or invent a height of zero.

For a browser app, call `inspectPending()` from the
[wallet example](#wallet-write-with-recovery). It needs only the
public client on the original network, so it works after reload or wallet
disconnect. `waitForTransactionReceipt` can resume polling but does not itself
require successful execution. Check `receipt.execResult` before showing success.

## Resolve an API disagreement

Start with the project's manifests, generated binding, and installed CLI help.
Then check the matching package's public declarations and usage guide. Inspect
implementation source when those surfaces do not answer the question. Report
the exact version and disagreement; do not combine incompatible APIs.

<!-- references/typescript.md -->

# TypeScript

Use the public client and generated contract bindings. A descriptor prepares or
decodes a call; it is not a connected contract instance.

Current RPC wire format 10 and CLI JSON encode all 64-bit-or-wider integers as
canonical decimal strings, including small values. Preserve them as `bigint`
in TypeScript; smaller bounded integers remain JSON numbers. Select a client
that supports the target's `wireFormat`. The examples below retain their tested
older dependency set and do not establish compatibility with wire format 10.

Pubkey accounts, including hosted schema-3 bootstrap accounts, use DefaultWallet.
Local schema-4 manifests contain public allocations; applications and harnesses
own their signing keys. Preserve historical source-address wallet identities
when using `devWalletAdapter` rather than deriving replacement addresses.

## Read a contract

Verified on 2026-10-05 with Node.js 22, `@groundhogchain/client` 0.14.0,
`@groundhogchain/protocol` 0.12.0, TypeScript 6.0.3, and tsx 4.22.4.
The exact read and wallet examples passed type checks and local-node execution
with CLI 0.22.1, including observation failure and reload recovery.
Configure [GitHub Packages access](https://github.com/groundhogchain/client-ts#registry-auth-one-time-setup),
then create a directory and install:

```sh
npm init -y
npm pkg set type=module
npm install --save-exact @groundhogchain/client@0.14.0 @groundhogchain/protocol@0.12.0
npm install --save-dev --save-exact typescript@6.0.3 tsx@4.22.4 @types/node@25.9.1
```

Start `groundhog node` in another terminal. This example reads the native HOG
contract on the local network; no wallet, private key, or contract deployment is
needed. For a hosted network, replace `localnet()` with its configured cluster.

```ts title="read.ts"
import { createGroundhogClient, localnet, formatUnits } from "@groundhogchain/client";
import { ERC20 } from "@groundhogchain/client/contracts";
import { HOG_ADDRESS } from "@groundhogchain/protocol";

async function main() {
  const client = await createGroundhogClient({ cluster: localnet() });
  const supply = await ERC20.v0_1.totalSupply.view({
    client,
    contract: HOG_ADDRESS,
  });
  console.log("HOG supply:", formatUnits(supply.value, 7));
  console.log("Read at block:", supply.height.toString());
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : "Read failed");
  process.exitCode = 1;
});
```

Run `npx tsx read.ts`. Expect a supply and a read height, not a fixed block number.
Keep amounts as `bigint`; `formatUnits` is for display. `parseUnits("0.0001", 7)`
produces `1000n` without rounding.

## Wallet write with recovery

Use the same client/protocol versions in a browser app on HTTPS or localhost.
The wallet and RPC must use the same local network. Fund the sending account and
use a recipient that already has HOG for this small-transfer example. A new
recipient also needs its balance-storage deposit.

For a Vite app, forward same-origin RPC requests to the local node. This avoids
depending on cross-origin access from the node's HTTP server:

```ts title="vite.config.ts"
import { defineConfig } from "vite";

export default defineConfig({
  server: { proxy: { "/rpc": { target: "http://127.0.0.1:9545", rewrite: () => "/" } } },
});
```

This single-tab module handles one unresolved transfer at a time. Call
`sendHog(provider, receiver)` from the user's send action. Obtain `provider`
through `discoverWalletProviders` from `@groundhogchain/client/provider` and
let the user choose it. The example uses the first account authorized by that
wallet. See the [wallet picker](https://docs.groundhog.dev/tools/wallet-integration/#discover-and-connect)
for account selection and discovery cleanup.

```ts title="wallet.ts"
import {
  addressFromHex, createGroundhogClient, gasAmount, hashFromHex, localnet,
  parseUnits, toHex0x, TransactionExecutionError, TransactionObservationError,
} from "@groundhogchain/client";
import { ERC20 } from "@groundhogchain/client/contracts";
import {
  parseSignTransactionResult, parseWalletAccount, parseWalletNetworkHash,
  parseWalletSession, type WalletProvider,
} from "@groundhogchain/client/provider";
import {
  createProviderWalletClient, type PendingWalletTransaction,
} from "@groundhogchain/client/wallet";
import { HOG_ADDRESS } from "@groundhogchain/protocol";

const client = await createGroundhogClient({
  cluster: localnet({ rpcUrl: new URL("/rpc", window.location.origin).href }),
});
const networkHash = toHex0x(client.network.networkHash).slice(2);
const storageKey = `hog-transfer:v1:${networkHash}`;
let sending = false;

function loadPending(): PendingWalletTransaction | null {
  const raw = localStorage.getItem(storageKey);
  if (raw === null) return null;
  const value: unknown = JSON.parse(raw);
  if (!value || typeof value !== "object") throw new Error("Invalid saved transfer");
  const record = value as Record<string, unknown>;
  if (record.version !== 1) throw new Error("Unsupported saved transfer version");
  const savedNetwork = parseWalletNetworkHash(record.networkHash);
  if (savedNetwork !== networkHash) throw new Error("Saved transfer is on another network");
  return {
    ...parseSignTransactionResult({ signedTransaction: record.signedTransaction, hash: record.hash }),
    account: parseWalletAccount(record.account),
    networkHash: savedNetwork,
  };
}

export async function sendHog(provider: WalletProvider, receiver: string) {
  if (sending || loadPending()) throw new Error("Inspect the existing transfer first");
  const recipient = addressFromHex(receiver);
  sending = true;
  try {
    const session = parseWalletSession(await provider.request({ method: "connect" }));
    if (session.networkHash !== networkHash) throw new Error("Select the app network");
    const account = session.accounts[0];
    if (!account) throw new Error("No authorized account");
    const wallet = createProviderWalletClient({ client, provider, account, networkHash });
    try {
      const invocation = ERC20.v0_1.transfer.invocation(
        HOG_ADDRESS, gasAmount(500_000n),
        { receiver: recipient, amount: parseUnits("0.0001", 7) },
      );
      const { receipt } = await wallet.executeAndWait(
        { invocation, gasLimit: gasAmount(1_000_000n) },
        {
          onSigned: (pending) => {
            if (loadPending()) throw new Error("An unresolved transfer is already saved");
            localStorage.setItem(storageKey, JSON.stringify({ version: 1, ...pending }));
          },
          wait: { timeoutMs: 30_000 },
        },
      );
      return { state: "committed" as const, receipt };
    } catch (error) {
      if (error instanceof TransactionExecutionError) {
        return { state: "committed" as const, receipt: error.receipt };
      }
      if (error instanceof TransactionObservationError) {
        return { state: "unobserved" as const, hash: toHex0x(error.txHash) };
      }
      throw error;
    } finally {
      wallet.dispose();
    }
  } finally {
    sending = false;
  }
}

// Call on reload or from a Check transaction button. No wallet connection needed.
export async function inspectPending() {
  const pending = loadPending();
  if (!pending) return null;
  const hash = hashFromHex(pending.hash);
  const status = await client.getTransactionStatus(hash);
  const receipt = status.status === "committed"
    ? await client.getTransactionReceipt(hash) : null;
  return { hash: pending.hash, account: pending.account.address, status, receipt };
}

// Call only after displaying the committed outcome and the user acknowledges it.
export async function acknowledgeResult() {
  const result = await inspectPending();
  if (!result?.receipt) throw new Error("The transfer outcome is still unresolved");
  localStorage.removeItem(storageKey);
}
```

The SDK awaits `onSigned` before broadcast. Storage failure stops submission.
The saved record contains signed bytes, hash, original account, and network,
not a private key. A rejected signature creates no new record. A session change
invalidates the adapter; it never selects another signer automatically.

On `unobserved`, keep the record and call `inspectPending()` later, including
after reload. If inspection fails, retain the record and show the error. Do not
call `sendHog` again to recover it. Check `receipt.execResult ===
ExecResult.ExecResultOK` (import `ExecResult` from the client) before showing
success: a committed receipt can describe failed execution. An unresolved or
corrupt saved record blocks new transfers rather than silently discarding evidence.

The gas values are budgets for this example, not universal defaults. Gas has no
token fee; free HOG supplies scheduling capacity and storage requires deposits.

## Generate bindings for your own contract

```sh
groundhog build --bindings ts
groundhog codegen ts target/groundhog/my_contract.spec.xdr -o src/my-contract.ts
```

Use the `.spec.xdr` from the deployed build. Regenerate after interface changes;
do not edit the generated file or maintain a second ABI. Select an interface
version such as `Counter.v0_1`. Use its `view` for reads and `invocation` for
wallet execution. `getContract` can bind a descriptor, address, and client once.

Use `executeCallsAndWait` when typed child return values are needed. Root receipt
bytes belong to the wallet, not directly to its child contract. A decoding failure
after successful execution is not a reason to repeat the write.

## Events and transport

- Generated event descriptors provide `filter`, `decode`, `allFromReceipt`, and
  `singleFromReceipt`. Use their codecs rather than manually encoding topics.
- Continue an opaque cursor with the same network, target, and filters. Stop on
  an empty returned cursor, not an empty page.
- Save event rows and progress together. For ascending ingestion, restart at the
  last saved row height and deduplicate by transaction hash and event sequence.
  A descending history page's oldest row is not a forward-ingestion checkpoint.
- `latestBlock` is the indexer head. Check `oldestBlock` for retained-history gaps.
- HTTP batching groups independent requests; it does not provide a shared
  snapshot, ordering, or atomicity. Await dependent operations separately.

See [event recovery](https://docs.groundhog.dev/typescript/events/#resume-without-missing-events)
and [transaction diagnostics](#inspect-the-original-transaction).
